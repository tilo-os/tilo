// Registered native RPC exercised against a real joined-room private owner.
struct PrivatePersonOwner(tilo_peer_state::owner::NativeWorkspaceOwner);
impl crate::services::person_chat::PersonChatServices for PrivatePersonOwner {
    fn command(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        use crate::services::person_chat::PersonChatCommand;
        let command: PersonChatCommand =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let room = tilo_desktop_model::RoomId::parse(room).ok_or("invalid room")?;
        let authority = tilo_peer_state::caller_authority::CallerAuthority::Person {
            caller: caller.clone(),
        };
        let now = std::time::SystemTime::now();
        let peer = match command {
            PersonChatCommand::Contacts => {
                let contacts = self.0.direct_contacts(room, &authority, now)?;
                return serde_json::to_vec(&contacts).map_err(|error| error.to_string());
            }
            PersonChatCommand::Open { public_key } => {
                self.0
                    .open_direct_conversation(room, &public_key, &authority, now)?;
                public_key
            }
            PersonChatCommand::Read { public_key } => public_key,
            PersonChatCommand::Send {
                public_key,
                text,
                invocation,
            } => {
                self.0.send_direct_message(
                    room,
                    &public_key,
                    &authority,
                    tilo_peer_state::owner::DirectMessageSend { invocation, text },
                    now,
                )?;
                public_key
            }
        };
        let conversation = self.0.read_direct_conversation(
            room,
            &peer,
            &authority,
            tilo_peer_state::owner::DirectConversationPage::default(),
            now,
        )?;
        serde_json::to_vec(&serde_json::json!({
            "peer": conversation.peer,
            "has_more": conversation.has_more,
            "messages": conversation.messages.into_iter().map(|message| serde_json::json!({
                "id": message.id,
                "author": message.sender,
                "text": message.text,
            })).collect::<Vec<_>>(),
        }))
        .map_err(|error| error.to_string())
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
            let room =
                tilo_desktop_model::RoomId::parse(tilo_peer_state::owner::testing::ROOM).unwrap();
            env.route.app_id = "so.tilo.chat".into();
            env.route.cluster_id = room.to_string();
            {
                let mut membership = env.membership.write();
                let before = membership.workspaces[0].clone();
                let mut workspace =
                    tilo_desktop_model::Workspace::in_room(before.id, room, before.name);
                workspace.strips = before.strips;
                workspace.policy = before.policy;
                workspace.policy.rehome(room.to_string());
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
            let request = br#"{"operation":"contacts"}"#;
            let actual: serde_json::Value =
                serde_json::from_slice(&worker.handle.person_chat(request, &[]).await.unwrap())
                    .unwrap();
            let expected = owner
                .direct_contacts(
                    room,
                    &tilo_peer_state::caller_authority::CallerAuthority::Person {
                        caller: env.cert.clone(),
                    },
                    std::time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(actual, serde_json::to_value(expected).unwrap());
            assert!(
                worker
                    .handle
                    .person_chat(br#"{"operation":"contacts","role":"person"}"#, &[])
                    .await
                    .is_err()
            );
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

/// A second certified person joined through the room's host, so the owner has
/// a private conversation peer that is a real member and not a bare label.
fn enroll_contact(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    governor: &mut tilo_peer_state::owner::testing::governor::TestGovernor,
    room: tilo_desktop_model::RoomId,
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
        iroh::EndpointId::from_bytes(&governor.room_host().device_public_key).unwrap(),
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
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    governor.room_host().device_public_key,
                ),
                message,
                now,
            )
            .unwrap();
    }
    let peer = hex::encode(certificate.user_public_key);
    assert!(
        owner
            .direct_contacts(
                room,
                &tilo_peer_state::caller_authority::CallerAuthority::Person {
                    caller: governor.room_host().clone(),
                },
                now,
            )
            .unwrap()
            .iter()
            .any(|contact| contact.public_key == peer),
        "the joined contact is the person's conversation peer"
    );
    peer
}

/// Everything the room publishes about `owner`'s state, as peers can read it.
fn public_owner_bytes(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
) -> Vec<Vec<u8>> {
    owner
        .with_current(room, |view| {
            let mut bytes = vec![tilo_peer_state::owner::testing::durable_peer(view).to_vec()];
            bytes.extend(tilo_peer_state::owner::testing::entries(view)?);
            bytes.extend(tilo_peer_state::owner::testing::projection(view)?);
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

/// The person's own open, send and read reach the real private store: later
/// reads return the body, and nothing of it reaches the public record.
#[tokio::test(flavor = "current_thread")]
async fn person_chat_open_send_read_stays_in_the_private_owner() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            let path = env.root.path().join("private-person-chat.json");
            let owner = tilo_peer_state::owner::open_file(&path).unwrap();
            let mut governor = tilo_peer_state::owner::testing::enroll_interactive_governor(
                &owner, &env.key, &env.cert,
            )
            .unwrap();
            let room =
                tilo_desktop_model::RoomId::parse(tilo_peer_state::owner::testing::ROOM).unwrap();
            let peer = enroll_contact(&owner, &mut governor, room);
            env.route.app_id = "so.tilo.chat".into();
            env.route.cluster_id = room.to_string();
            {
                let mut membership = env.membership.write();
                let before = membership.workspaces[0].clone();
                let mut workspace =
                    tilo_desktop_model::Workspace::in_room(before.id, room, before.name);
                workspace.strips = before.strips;
                workspace.policy = before.policy;
                workspace.policy.rehome(room.to_string());
                membership.workspaces[0] = workspace;
            }
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let public_before = public_owner_bytes(&owner, room);
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
            let opened = person_chat(
                handle,
                serde_json::json!({"operation": "open", "public_key": peer}),
            )
            .await
            .unwrap();
            assert_eq!(opened["peer"], peer);
            assert!(
                opened["messages"].as_array().unwrap().is_empty(),
                "{opened}"
            );
            let marker = "person-chat-private-body-canary";
            let sent = person_chat(
                handle,
                serde_json::json!({
                    "operation": "send",
                    "public_key": peer,
                    "text": marker,
                    "invocation": uuid::Uuid::new_v4(),
                }),
            )
            .await
            .unwrap();
            assert_eq!(sent["peer"], peer);
            let read = person_chat(
                handle,
                serde_json::json!({"operation": "read", "public_key": peer}),
            )
            .await
            .unwrap();
            assert_eq!(read["messages"].as_array().unwrap().len(), 1, "{read}");
            assert_eq!(read["messages"][0]["text"], marker);
            assert_eq!(
                read["messages"][0]["author"],
                hex::encode(env.cert.user_public_key)
            );
            assert_eq!(
                public_owner_bytes(&owner, room),
                public_before,
                "a private conversation writes nothing to the public room"
            );
            for bytes in public_owner_bytes(&owner, room) {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "the message body stays out of the published bytes"
                );
            }
            let saved = owner
                .read_direct_conversation(
                    room,
                    &peer,
                    &tilo_peer_state::caller_authority::CallerAuthority::Person {
                        caller: env.cert.clone(),
                    },
                    tilo_peer_state::owner::DirectConversationPage::default(),
                    std::time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(saved.messages.len(), 1);
            assert_eq!(saved.messages[0].text, marker);
            assert_eq!(
                saved.messages[0].sender,
                hex::encode(env.cert.user_public_key)
            );
            drop(worker);
            // The connection's tasks hold the owner too; an aborted task is
            // dropped on the next turn, before its store is opened again.
            tokio::task::yield_now().await;
            state.stop();
            drop(state);
            drop(owner);
            let reopened = tilo_peer_state::owner::open_file(&path).unwrap();
            let persisted = reopened
                .read_direct_conversation(
                    room,
                    &peer,
                    &tilo_peer_state::caller_authority::CallerAuthority::Person {
                        caller: env.cert.clone(),
                    },
                    tilo_peer_state::owner::DirectConversationPage::default(),
                    std::time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(
                persisted.messages, saved.messages,
                "the send passed the owner's store barrier"
            );
        })
        .await;
}
