// Source coverage of the actual certified native command path (not a mock store).
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

fn private_editor_public_snapshot(
    env: &Environment,
    editor: &runtime_collab::private_draft::Editor,
) -> (Vec<u8>, Vec<u8>) {
    // Reopen the real committed owner, rather than serializing a hand-built
    // projection. Also inspect decoded room values: Automerge may compress text.
    let owner = crate::test_owner::SessionOwner::open(&env.root.path().join("owner")).unwrap();
    let room = owner.peer().admitted_room().unwrap();
    assert_private_editor_absent(
        &serde_json::to_vec(&automerge::AutoSerde::from(room)).unwrap(),
        editor,
    );
    for change in room.clone().get_changes(&[]) {
        assert_private_editor_absent(change.raw_bytes(), editor);
    }
    let writer = owner
        .peer()
        .document(crate::test_owner::WRITER.0, crate::test_owner::WRITER.1)
        .unwrap();
    let mut writer = automerge::AutoCommit::load(&writer).unwrap();
    assert_private_editor_absent(
        &serde_json::to_vec(&automerge::AutoSerde::from(&writer)).unwrap(),
        editor,
    );
    for change in writer.get_changes(&[]) {
        assert_private_editor_absent(change.raw_bytes(), editor);
    }
    let durable = owner.peer().durable().unwrap();
    let publications = owner.outgoing().to_vec();
    assert!(!durable.is_empty());
    assert!(
        !publications.is_empty(),
        "the real owner must publish a checkpoint"
    );
    assert_private_editor_absent(&durable, editor);
    for publication in &publications {
        let _: neighbour_wire::NeighbourMsg = postcard::from_bytes(publication).unwrap();
        assert_private_editor_absent(publication, editor);
    }
    let membership = serde_json::to_vec(&env.membership.read().desktop).unwrap();
    assert_private_editor_absent(&membership, editor);
    // Includes the owner session and the broker's actual on-disk invocation journal.
    assert_private_editor_absent_from_store(env.root.path(), editor);
    // Reopening regenerates signed bootstrap envelopes with fresh timestamps.
    // Compare the committed store bytes, not those newly signed envelopes.
    (
        std::fs::read(env.root.path().join("owner/session.bin")).unwrap(),
        membership,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn private_editor_staging_and_adoption_leave_real_room_publication_and_history_private() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            env.publisher.owner_commit_health().unwrap();
            let rig = Rig::new(&env).await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let proof = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let draft = runtime_collab::private_draft::Draft {
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
            };
            let before = private_editor_public_snapshot(&env, &draft.editor);
            let token = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .unwrap();
            assert!(
                rig.state
                    .private_drafts
                    .lock()
                    .unwrap()
                    .contains_key(&token)
            );
            assert_eq!(private_editor_public_snapshot(&env, &draft.editor), before);
            let handle = &rig.worker.handle;
            handle
                .publish_action_directory(
                    "so.tilo.chat",
                    capnp_rpc::new_client(PrivateDraftDirectory),
                )
                .await
                .unwrap();
            let (_, binding) = handle
                .resolve_published_action_for_invocation(
                    903,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                )
                .await
                .unwrap();
            let invocation = id(0xd2af8);
            let argument = serde_json::json!({"token": token});
            handle
                .invoke_resolved_published_action_durable(
                    invocation.0.as_bytes(),
                    &binding,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                    Some(&argument.to_string()),
                    "{}",
                )
                .await
                .unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *invocation.0.as_bytes());
            assert_eq!(delivered.action, "adopt-private-draft");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(delivered.argument.as_deref().unwrap())
                    .unwrap(),
                argument,
                "the delivered action must contain exactly the opaque token"
            );
            let (key, context) = {
                let pending = rig.state.pending_invocations.lock().unwrap();
                let pending = pending.get(&delivered.id).unwrap();
                (
                    pending.journal_key.clone().unwrap(),
                    pending.context.clone(),
                )
            };
            let tilo_actions::Did::Verb(verb) = context.what.as_ref().unwrap() else {
                panic!("the real dispatcher must generate a verb history entry");
            };
            assert_eq!(verb.action.action, "adopt-private-draft");
            // View history deliberately keeps no argument values, not even the token.
            assert!(verb.args.is_empty());
            assert_private_editor_absent(&serde_json::to_vec(&context).unwrap(), &draft.editor);
            {
                let journal = rig.state.journal.lock().unwrap();
                let stored = journal
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .lookup(&key)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    stored.intent["context"],
                    serde_json::to_value(&context).unwrap()
                );
                assert_private_editor_absent(
                    &serde_json::to_vec(&stored.intent).unwrap(),
                    &draft.editor,
                );
                assert!(stored.admitted_request_is(&serde_json::json!({
                    "binding": {
                        "host": binding.host, "peer_id": binding.peer_id,
                        "app_id": binding.app_id, "record_id": binding.record_id,
                        "root": binding.root, "device": binding.device,
                        "target_binding": binding.target_binding,
                    },
                    "revision": "draft-r1", "resource": "workspace:atlas",
                    "action": "adopt-private-draft", "argument": argument.to_string(),
                })));
            }
            let request = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: false,
            };
            let bytes = handle
                .private_draft(&request, invocation.0.as_bytes())
                .await
                .unwrap();
            let fetched: runtime_collab::private_draft::Draft =
                serde_json::from_slice(&bytes).unwrap();
            assert!(fetched.editor == draft.editor);
            assert!(fetched.target == draft.target);
            assert_eq!(fetched.workspace, draft.workspace);
            assert_eq!(private_editor_public_snapshot(&env, &draft.editor), before);
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
            {
                let journal = rig.state.journal.lock().unwrap();
                let stored = journal
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .lookup(&key)
                    .unwrap()
                    .unwrap();
                assert_eq!(stored.update, update);
                assert_private_editor_absent(
                    &serde_json::to_vec(&stored.intent).unwrap(),
                    &draft.editor,
                );
            }
            // Adoption is a View, not a send: neither room state nor published
            // history may change, even after its actual terminal journal write.
            assert_eq!(private_editor_public_snapshot(&env, &draft.editor), before);
            rig.stop().await;
        })
        .await;
}

struct PrivateDraftDirectory;
impl runtime_collab::raw::action_directory::Server for PrivateDraftDirectory {
    fn resolve(
        self: Rc<Self>,
        _params: runtime_collab::raw::action_directory::ResolveParams,
        mut results: runtime_collab::raw::action_directory::ResolveResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let mut row = directory_state("draft-r1").borrow().row();
        row.action = Some("adopt-private-draft".into());
        row.descriptor.as_mut().unwrap().action = "adopt-private-draft".into();
        row.visibility.as_mut().unwrap().action = "adopt-private-draft".into();
        capnp::capability::Promise::from(runtime_collab::write_action_directory_row(
            results.get().init_resolution().init_found(),
            &row,
        ))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn private_editor_token_is_bounded_original_person_and_worker_scoped() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let rig = Rig::new(&env).await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let proof = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let draft = runtime_collab::private_draft::Draft {
                workspace: env.route.cluster_id.clone(),
                target: runtime_collab::private_draft::Target::Person {
                    public_key: "a".repeat(64),
                },
                editor: runtime_collab::private_draft::Editor {
                    text: "unsent private body é".into(),
                    anchor: 7,
                    caret: 3,
                    preedit: "候補".into(),
                    preedit_cursor: Some((0, 3)),
                },
            };
            let token = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .unwrap();
            for _ in 1..16 {
                rig.state
                    .stage_private_draft(&caller, &proof, draft.clone())
                    .unwrap();
            }
            assert!(
                rig.state
                    .stage_private_draft(&caller, &proof, draft.clone())
                    .is_err()
            );
            let handle = &rig.worker.handle;
            handle
                .publish_action_directory(
                    "so.tilo.chat",
                    capnp_rpc::new_client(PrivateDraftDirectory),
                )
                .await
                .unwrap();
            let (_, binding) = handle
                .resolve_published_action_for_invocation(
                    903,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                )
                .await
                .unwrap();
            let original = id(0xd2af7);
            let argument = serde_json::json!({"token": token}).to_string();
            handle
                .invoke_resolved_published_action_durable(
                    original.0.as_bytes(),
                    &binding,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                    Some(&argument),
                    "{}",
                )
                .await
                .unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let request = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: false,
            };
            assert!(handle.private_draft(&request, &[]).await.is_err());
            assert!(
                handle
                    .private_draft(&request, uuid::Uuid::new_v4().as_bytes())
                    .await
                    .is_err()
            );
            let bytes = handle
                .private_draft(&request, original.0.as_bytes())
                .await
                .unwrap();
            let fetched: runtime_collab::private_draft::Draft =
                serde_json::from_slice(&bytes).unwrap();
            assert!(fetched.editor == draft.editor);
            assert!(
                rig.state
                    .private_drafts
                    .lock()
                    .unwrap()
                    .contains_key(&token),
                "fetch is not consumption"
            );
            let wrong = runtime_collab::private_draft::Request {
                private_draft: uuid::Uuid::new_v4(),
                acknowledge: true,
            };
            assert!(
                handle
                    .private_draft(&wrong, original.0.as_bytes())
                    .await
                    .is_err()
            );
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
                    .private_draft(&request, original.0.as_bytes())
                    .await
                    .is_err()
            );
            rig.state
                .pending_invocations
                .lock()
                .unwrap()
                .get_mut(&delivered.id)
                .unwrap()
                .owner = old_owner;
            let ack = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: true,
            };
            assert_eq!(
                handle
                    .private_draft(&ack, original.0.as_bytes())
                    .await
                    .unwrap(),
                b"null"
            );
            assert!(
                handle
                    .private_draft(&request, original.0.as_bytes())
                    .await
                    .is_err()
            );
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
                    .contains_key(&expired)
            );
            *env.authority.certificate.lock().unwrap() = None;
            assert!(
                rig.state
                    .stage_private_draft(&caller, &proof, draft)
                    .is_err()
            );
        })
        .await;
}

/// One Chat window holding several conversations of one room: the draft
/// belongs to the conversation its adopt action is delivered to, so that
/// record's channel fetches it and a sibling record's channel does not.
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

            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let draft = runtime_collab::private_draft::Draft {
                workspace: env.route.cluster_id.clone(),
                target: runtime_collab::private_draft::Target::Person {
                    public_key: "c".repeat(64),
                },
                editor: runtime_collab::private_draft::Editor {
                    text: "draft for the second conversation".into(),
                    anchor: 0,
                    caret: 5,
                    preedit: String::new(),
                    preedit_cursor: None,
                },
            };
            let token = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .expect("a person with Chat open across several records may stage a draft");

            second
                .publish_action_directory(
                    "so.tilo.chat",
                    capnp_rpc::new_client(PrivateDraftDirectory),
                )
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
            let (_, binding) = second
                .resolve_published_action_for_invocation(
                    second_hold,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                )
                .await
                .unwrap();
            let invocation = id(0xd2af9);
            second
                .invoke_resolved_published_action_durable(
                    invocation.0.as_bytes(),
                    &binding,
                    "draft-r1",
                    "workspace:atlas",
                    "adopt-private-draft",
                    Some(&serde_json::json!({"token": token}).to_string()),
                    "{}",
                )
                .await
                .unwrap();
            let delivered = stream.read().await.unwrap();
            assert_eq!(delivered.invocation_uuid, *invocation.0.as_bytes());
            let request = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: false,
            };
            assert!(
                first
                    .private_draft(&request, invocation.0.as_bytes())
                    .await
                    .is_err(),
                "a sibling conversation's channel does not hold this draft"
            );
            let fetched: runtime_collab::private_draft::Draft = serde_json::from_slice(
                &second
                    .private_draft(&request, invocation.0.as_bytes())
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
