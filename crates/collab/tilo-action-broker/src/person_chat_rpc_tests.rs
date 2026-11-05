// Registered native RPC exercised against a real joined-workspace private owner.
struct PrivatePersonOwner(tilo_peer_state::owner::NativeWorkspaceOwner);
impl crate::services::person_chat::PersonChatServices for PrivatePersonOwner {
    fn command(
        &self,
        workspace: &str,
        caller: &tilo_user_identity::DeviceCert,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        use crate::services::person_chat::PersonChatCommand;
        let command: PersonChatCommand =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let workspace =
            tilo_desktop_model::WorkspaceId::parse(workspace).ok_or("invalid workspace")?;
        let authority = tilo_peer_state::caller_authority::CallerAuthority::Person {
            caller: caller.clone(),
        };
        let now = std::time::SystemTime::now();
        let value = match command {
            PersonChatCommand::Import {
                public_key,
                after_sequence,
            } => {
                let page = tilo_peer_state::owner::OldConversationPage {
                    after_sequence,
                    limit: 50,
                };
                // The door names people as the workspace does; the old store
                // that kept their conversations spells them by their hash.
                let stored = tilo_peer_state::owner::person_store_key(
                    hex::decode(&public_key)
                        .ok()
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                        .ok_or("invalid conversation peer")?,
                );
                let conversation = self
                    .0
                    .old_direct_conversation(workspace, &stored, &authority, page, now)?;
                // A person with no old conversation has nothing to import.
                let Some(conversation) = conversation else {
                    return serde_json::to_vec(&serde_json::json!({
                        "peer": public_key,
                        "has_more": false,
                        "next_sequence": after_sequence,
                        "messages": [],
                    }))
                    .map_err(|error| error.to_string());
                };
                serde_json::json!({
                    "peer": public_key,
                    "has_more": conversation.has_more,
                    "next_sequence": conversation.next_sequence,
                    "messages": conversation.messages.into_iter().map(|message| serde_json::json!({
                        "id": message.id,
                        "author": message.sender,
                        "text": message.text,
                        "at": message.received_at,
                    })).collect::<Vec<_>>(),
                })
            }
            PersonChatCommand::Clear => {
                self.0
                    .clear_old_direct_conversations(workspace, &authority, now)?;
                serde_json::json!({})
            }
        };
        serde_json::to_vec(&value).map_err(|error| error.to_string())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn person_rpc_uses_bound_owner_and_refuses_lost_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            let owner = tilo_peer_state::owner::open_file(
                &env.root.path().join("private-person-owner.json"),
            )
            .unwrap();
            tilo_peer_state::owner::testing::enroll_interactive_governor(
                &owner, &env.key, &env.cert,
            )
            .unwrap();
            let workspace_id =
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap();
            env.route.app_id = "so.tilo.chat".into();
            env.route.cluster_id = workspace_id.to_string();
            {
                let mut membership = env.membership.write();
                let before = membership.workspaces[0].clone();
                let mut workspace = tilo_desktop_model::Workspace::in_workspace(
                    before.slot,
                    workspace_id,
                    before.name,
                );
                workspace.strips = before.strips;
                workspace.policy = before.policy;
                workspace.policy.rehome(workspace_id.to_string());
                membership.workspaces[0] = workspace;
            }
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let mut services = env.membership.services();
            services.person_chat = Some(Arc::new(PrivatePersonOwner(owner.clone())));
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.authority()),
                None,
                None,
                services,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
            let (worker, _) =
                join_worker(&state, &directories, &env.route, &env.cert, &env.key, 451).await;
            let request = br#"{"operation":"clear"}"#;
            let actual: serde_json::Value =
                serde_json::from_slice(&worker.handle.person_chat(request, &[]).await.unwrap())
                    .unwrap();
            assert_eq!(actual, serde_json::json!({}));
            assert!(
                worker
                    .handle
                    .person_chat(br#"{"operation":"clear","role":"person"}"#, &[])
                    .await
                    .is_err()
            );
            // Nothing an action carries is answered here: no invocation is
            // the original person of a window's own private snapshot.
            assert!(
                worker
                    .handle
                    .person_chat(request, uuid::Uuid::new_v4().as_bytes())
                    .await
                    .is_err()
            );
            assert!(worker.handle.person_chat(request, &[0; 15]).await.is_err());
            // A descriptor without exact private app registration cannot borrow it.
            let socket = state
                .connect_local_worker(env.route.clone(), &env.cert)
                .unwrap();
            let pending = state.workers.lock().unwrap().pop().unwrap();
            let (rpc, server, client) = rpc_pair(
                state.clone(),
                452,
                pending.stream,
                socket,
                Some(pending.binding),
                directories.clone(),
            );
            let ordinary = rpc
                .join_channel(
                    &env.route.channel_key(),
                    &presentation(&env.route.cluster_id, &env.cert, &env.key),
                )
                .await
                .unwrap();
            assert!(ordinary.person_chat(request, &[]).await.is_err());
            *env.authority.certificate.lock().unwrap() = None;
            assert!(worker.handle.person_chat(request, &[]).await.is_err());
            server.abort();
            client.abort();
            let _ = server.await;
            let _ = client.await;
        })
        .await;
}

/// A second certified person joined through the workspace's host, so the owner has
/// a private conversation peer that is a real member and not a bare label.
fn enroll_contact(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    governor: &mut tilo_peer_state::owner::testing::governor::TestGovernor,
    workspace: tilo_desktop_model::WorkspaceId,
) -> String {
    use tilo_peer_state::join;
    let now = std::time::SystemTime::now();
    let key = SigningKey::from_bytes(&[73; 32]);
    let certificate = tilo_peer_state::owner::testing::certificate(
        &SigningKey::from_bytes(&[74; 32]),
        &key,
        "Conversation peer",
    );
    let ns = tilo_peer_state::owner::testing::ns(now).unwrap();
    let hint = governor.hint(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.workspace_host().device_public_key).unwrap(),
    ));
    let invitation = governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        Some("Conversation peer".into()),
        ns + 60_000_000_000,
    );
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id: [75; 32],
        }),
    );
    let challenge = match response {
        join::JoinTransportResponse::Challenge(challenge) => Some(challenge),
        _ => None,
    }
    .expect("the certified contact receives a join challenge");
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Complete(join::JoinCompleteRequest {
            proof: join::sign_join_proof(challenge.challenge, &key).unwrap(),
        }),
    );
    assert!(matches!(response, join::JoinTransportResponse::Complete(_)));
    for (bytes, _) in governor.outgoing() {
        let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        owner
            .receive(
                workspace,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    governor.workspace_host().device_public_key,
                ),
                message,
                Some(&key),
                now,
            )
            .unwrap();
    }
    hex::encode(certificate.user_public_key)
}

/// Everything the workspace publishes about `owner`'s state, as peers can read
/// it: the record's durable peer and the outgoing frames still in custody.
fn public_owner_bytes(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
) -> Vec<Vec<u8>> {
    owner
        .with_current(workspace, |view| {
            let mut bytes = vec![tilo_peer_state::owner::testing::durable_peer(view).to_vec()];
            bytes.extend(tilo_peer_state::owner::testing::entries(view)?);
            Ok(bytes)
        })
        .unwrap()
}

async fn person_chat(
    handle: &runtime_collab::ChannelHandle,
    operation: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request = operation.to_string().into_bytes();
    let answer = handle
        .person_chat(&request, &[])
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&answer).map_err(|error| error.to_string())
}

/// A person's old store is read page by page through the door and emptied
/// once: the body never reaches the public workspace, and nothing writes
/// there again.
#[tokio::test(flavor = "current_thread")]
async fn person_rpc_imports_the_old_store_and_empties_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            let path = env.root.path().join("private-person-chat.json");
            let owner = tilo_peer_state::owner::open_file(&path).unwrap();
            let mut governor = tilo_peer_state::owner::testing::enroll_interactive_governor(
                &owner, &env.key, &env.cert,
            )
            .unwrap();
            let workspace_id =
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap();
            let peer = enroll_contact(&owner, &mut governor, workspace_id);
            // What a device's older session left in the old store.
            let marker = "person-chat-private-body-canary";
            let authority = tilo_peer_state::caller_authority::CallerAuthority::Person {
                caller: env.cert.clone(),
            };
            let now = std::time::SystemTime::now();
            owner
                .seed_old_direct_conversations(workspace_id, &peer, &[marker], &[], now)
                .unwrap();
            env.route.app_id = "so.tilo.chat".into();
            env.route.cluster_id = workspace_id.to_string();
            {
                let mut membership = env.membership.write();
                let before = membership.workspaces[0].clone();
                let mut workspace = tilo_desktop_model::Workspace::in_workspace(
                    before.slot,
                    workspace_id,
                    before.name,
                );
                workspace.strips = before.strips;
                workspace.policy = before.policy;
                workspace.policy.rehome(workspace_id.to_string());
                membership.workspaces[0] = workspace;
            }
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let public_before = public_owner_bytes(&owner, workspace_id);
            let mut services = env.membership.services();
            services.person_chat = Some(Arc::new(PrivatePersonOwner(owner.clone())));
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.authority()),
                None,
                None,
                services,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
            let (worker, _) =
                join_worker(&state, &directories, &env.route, &env.cert, &env.key, 453).await;
            let handle = &worker.handle;
            let imported = person_chat(
                handle,
                serde_json::json!({
                    "operation": "import",
                    "public_key": peer,
                    "after_sequence": 0,
                }),
            )
            .await
            .unwrap();
            assert_eq!(imported["peer"], peer);
            assert_eq!(imported["has_more"], false);
            assert_eq!(
                imported["messages"].as_array().unwrap().len(),
                1,
                "{imported}"
            );
            assert_eq!(imported["messages"][0]["text"], marker);
            assert_eq!(
                imported["messages"][0]["author"],
                tilo_peer_state::owner::person_store_key(env.cert.user_public_key),
                "the message says the person wrote it"
            );
            assert_eq!(
                public_owner_bytes(&owner, workspace_id),
                public_before,
                "reading the old store writes nothing to the public workspace"
            );
            for bytes in public_owner_bytes(&owner, workspace_id) {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "the message body stays out of the published bytes"
                );
            }
            assert_eq!(
                person_chat(
                    handle,
                    serde_json::json!({
                        "operation": "clear",
                    }),
                )
                .await
                .unwrap(),
                serde_json::json!({})
            );
            let emptied = person_chat(
                handle,
                serde_json::json!({
                    "operation": "import",
                    "public_key": peer,
                    "after_sequence": 0,
                }),
            )
            .await
            .unwrap();
            assert_eq!(
                emptied["messages"].as_array().unwrap().len(),
                0,
                "the old store is empty: {emptied}"
            );
            assert_eq!(emptied["peer"], peer);
            drop(worker);
            // The connection's tasks hold the owner too; an aborted task is
            // dropped on the next turn, before its store is opened again.
            tokio::task::yield_now().await;
            state.stop();
            drop(state);
            drop(owner);
            let reopened = tilo_peer_state::owner::open_file(&path).unwrap();
            assert!(
                reopened
                    .old_direct_conversation(
                        workspace_id,
                        &tilo_peer_state::owner::person_store_key(
                            hex::decode(&peer)
                                .ok()
                                .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                                .unwrap(),
                        ),
                        &authority,
                        tilo_peer_state::owner::OldConversationPage::default(),
                        std::time::SystemTime::now(),
                    )
                    .unwrap()
                    .is_none(),
                "the emptying passed the owner's store barrier"
            );
        })
        .await;
}
