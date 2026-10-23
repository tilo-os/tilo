// Registered native RPC exercised against a real joined-room private owner.
struct PrivatePersonOwner(tilo_joined_room::NativeWorkspaceOwner);
impl crate::services::person_chat::PersonChatServices for PrivatePersonOwner {
    fn command(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let command: crate::services::person_chat::PersonChatCommand =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if !matches!(
            command,
            crate::services::person_chat::PersonChatCommand::Contacts
        ) {
            return Err("this test exercises the real owner contact lookup only".into());
        }
        let contacts = self.0.direct_contacts(
            tilo_desktop_model::RoomId::parse(room).ok_or("invalid room")?,
            &tilo_peer_state::caller_authority::CallerAuthority::Person {
                caller: caller.clone(),
            },
            std::time::SystemTime::now(),
        )?;
        serde_json::to_vec(&contacts).map_err(|error| error.to_string())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn person_rpc_uses_bound_owner_and_refuses_lost_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            let owner =
                tilo_joined_room::open_file(&env.root.path().join("private-person-owner.json"))
                    .unwrap();
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &env.key, &env.cert)
                .unwrap();
            let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
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
            let (state, bridge) = CapnpBrokerState::new_configured(
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
