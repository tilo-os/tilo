// Exercise the production registered SDK surface, not a command collector.
use super::*;

/// Only adapts wire DTOs: all authorization, messages and subscriptions belong
/// to the real joined-room owner (whose Store enforces the persistence barrier).
struct SdkPersonOwner(tilo_joined_room::NativeWorkspaceOwner);
impl crate::services::person_chat::PersonChatServices for SdkPersonOwner {
    fn command(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        use crate::services::person_chat::PersonChatCommand;
        let room = tilo_desktop_model::RoomId::parse(room).ok_or("invalid room")?;
        let authority = tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person {
            caller: caller.clone(),
        };
        let now = std::time::SystemTime::now();
        let command = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let peer = match command {
            PersonChatCommand::Contacts => {
                let contacts = self.0.direct_contacts(room, &authority, now)?;
                return serde_json::to_vec(
                    &contacts
                        .into_iter()
                        .map(|contact| {
                            serde_json::json!({"public_key":contact.public_key,
                        "display_name":contact.display_name.unwrap_or_default()})
                        })
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| error.to_string());
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
                    tilo_joined_room::DirectMessageSend { invocation, text },
                    now,
                )?;
                public_key
            }
        };
        let conversation = self.0.read_direct_conversation(
            room,
            &peer,
            &authority,
            tilo_joined_room::DirectConversationPage::default(),
            now,
        )?;
        assert!(!conversation.has_more, "the fixture contains one message");
        serde_json::to_vec(&serde_json::json!({"peer":conversation.peer,
            "messages":conversation.messages.into_iter().map(|message| {
                serde_json::json!({"id":message.id,"author":message.sender,"text":message.text})
            }).collect::<Vec<_>>()
        }))
        .map_err(|error| error.to_string())
    }

    fn subscribe(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        self.0.subscribe_direct_conversation(
            tilo_desktop_model::RoomId::parse(room).ok_or("invalid room")?,
            peer,
            &tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            std::time::SystemTime::now(),
        )
    }
}

fn enroll_contact(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    governor: &mut tilo_joined_room::testing::governor::TestGovernor,
    room: tilo_desktop_model::RoomId,
    now: std::time::SystemTime,
) -> Result<(), String> {
    use tilo_peer_state::join;
    let key = SigningKey::from_bytes(&[73; 32]);
    let certificate = tilo_joined_room::testing::certificate(
        &SigningKey::from_bytes(&[74; 32]),
        &key,
        "Conversation peer",
    );
    let ns = tilo_joined_room::testing::ns(now).unwrap();
    let hint = governor.hint(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.governor().device_public_key).unwrap(),
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
    let join::JoinTransportResponse::Challenge(challenge) = response else {
        return Err("certified contact challenge refused".into());
    };
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
                    governor.governor().device_public_key,
                ),
                message,
                now,
            )
            .unwrap();
    }
    assert!(
        owner
            .direct_contacts(
                room,
                &tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person {
                    caller: governor.governor().clone(),
                },
                now
            )
            .unwrap()
            .iter()
            .any(|contact| contact.public_key == hex::encode(certificate.user_public_key))
    );
    Ok(())
}

fn chat_seed(room: &str) -> Vec<tilo_peer_state::SeedDocument> {
    let definition = app_framework::framework::definition::<chat::ChatShell>();
    let record = ui::record::RecordId::for_tab(1);
    let mut document = automerge::AutoCommit::new();
    assert!(ui::record::init_scoped(
        &mut document,
        &record,
        &definition.record_kind,
        room,
        Some(room)
    ));
    vec![tilo_peer_state::SeedDocument {
        app_id: definition.app_id().into(),
        record_id: record.as_str().into(),
        kind: definition.record_kind.as_str().into(),
        bytes: document.save(),
    }]
}

fn mount_chat(
    route: &CrdtRoute,
    certificate: &tilo_user_identity::DeviceCert,
    manager: ui::collaboration::CollaborationManager,
) -> ui::App {
    use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
    use ui::portable_storage::{PortableStorage, StorageKey};
    use ui::record::{RecordCheckout, RecordId, RecordOwners};
    let definition = app_framework::framework::definition::<chat::ChatShell>();
    let record = RecordId::parse(&route.record_id).unwrap();
    let mut initial = automerge::AutoCommit::new();
    assert!(ui::record::init_scoped(
        &mut initial,
        &record,
        &definition.record_kind,
        &route.cluster_id,
        Some(&route.cluster_id)
    ));
    let cache = PortableStorage::new("chat-private-owner-proof", "device").unwrap();
    cache
        .hydrate(
            StorageKey::Record {
                package: definition.app_id().into(),
                workspace: route.cluster_id.clone(),
                record: record.as_str().into(),
            },
            initial.save(),
            Some(certificate.device_public_key.to_vec()),
        )
        .unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let records = owners.open(definition.app_id(), &route.cluster_id).unwrap();
    let views = Rc::new(
        ui::application_view::InstanceViewStore::portable(cache, &route.cluster_id).unwrap(),
    );
    let target = LaunchTarget::Record {
        checkout: RecordCheckout::new(record, definition.record_kind.clone()),
        reveal: None,
    };
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: "device".into(),
            instance_id: 1,
            permissions: Rc::new(RefCell::new(
                ui::permissions::PermissionContext::permissive(),
            )),
            author: ui::permissions::UserId(principal_id_for_root(certificate.user_public_key)),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    ui::App::from_component_with_collaboration(
        component,
        ui::application::ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
        manager,
    )
    .unwrap()
}

struct ChatSurface;
impl ui::app::AppSurface for ChatSurface {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(1024, 768)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

fn public_owner_bytes(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
) -> Vec<Vec<u8>> {
    owner
        .with_current(room, |view| {
            let mut bytes = vec![view.peer().durable()?];
            bytes.extend(tilo_joined_room::testing::entries(view)?);
            bytes.extend(tilo_joined_room::testing::projection(view)?);
            Ok(bytes)
        })
        .unwrap()
}

struct SdkContext {
    env: ProvisionedEnvironment,
    owner: tilo_joined_room::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
    path: std::path::PathBuf,
    authority: tilo_peer_state::workspace_task::WorkspaceTaskAuthority,
    peer: String,
    public_before: Vec<Vec<u8>>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
}
fn sdk_context() -> SdkContext {
    use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
    let mut env = Environment::with_owner_initializer(|path| {
        crate::test_owner::SessionOwner::initialize_in_room(
            path,
            tilo_joined_room::testing::ROOM,
            |room| Ok(chat_seed(room)),
        )
    });
    let path = env.root.path().join("sdk-private-owner.json");
    let owner = tilo_joined_room::open_file(&path).unwrap();
    let mut governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &env.key, &env.cert)
            .unwrap();
    let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let now = std::time::SystemTime::now();
    enroll_contact(&owner, &mut governor, room, now).unwrap();
    env.route = CrdtRoute::record(
        "so.tilo.chat",
        room.to_string(),
        ui::record::RecordId::for_tab(1).as_str(),
    );
    {
        let mut tree = env.membership.write();
        let workspace = tree.workspaces.first_mut().expect("initial workspace");
        workspace.set_room(room);
        let principal =
            tilo_action_policy::Subject::Person(principal_id_for_root(env.cert.user_public_key));
        workspace.grant(tilo_action_policy::Grant::new(
            "certified-chat-member",
            principal.clone(),
            workspace.node(),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            principal,
        ));
    }
    env.authority = CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
    let authority = WorkspaceTaskAuthority::Person {
        caller: env.cert.clone(),
    };
    let contacts = owner.direct_contacts(room, &authority, now).unwrap();
    let peer = contacts
        .first()
        .expect("a second certified person, not an arbitrary label")
        .public_key
        .clone();
    let public_before = public_owner_bytes(&owner, room);
    let mut services = env.membership.services();
    services.person_chat = Some(Arc::new(SdkPersonOwner(owner.clone())));
    let settings_path = env.root.path().join("settings.json");
    services.settings = Some(Arc::new(settings_store::HostDocument::new(
        serde_json::Map::default(),
        move |settings| std::fs::write(&settings_path, serde_json::to_vec(settings)?),
    )));
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

    SdkContext {
        env,
        owner,
        room,
        path,
        authority,
        peer,
        public_before,
        state,
        directories,
    }
}
struct StopOnDrop(Arc<AtomicBool>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn mount_sdk_chat(
    env: &Environment,
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
) -> (
    tokio::task::JoinHandle<String>,
    tokio::task::JoinHandle<Result<(), capnp::Error>>,
    StopOnDrop,
) {
    use std::os::fd::IntoRawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
    let (socket, _authorize) = state
        .connect_local_worker_with_authorizer(env.route.clone(), &env.cert)
        .unwrap();
    let pending = state.workers.lock().unwrap().pop().unwrap();
    pending.stream.set_nonblocking(true).unwrap();
    let server = tokio::task::spawn_local(drive_connection(
        tokio::net::UnixStream::from_std(pending.stream).unwrap(),
        state.clone(),
        directories.clone(),
        461,
        Some(pending.binding),
    ));
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = StopOnDrop(stopped.clone());
    let route = env.route.clone();
    let certificate = env.cert.clone();
    let identity = CertifiedEndpointIdentity::new(
        presentation(&route.cluster_id, &certificate, &env.key),
        env.key.clone(),
    );
    // SDK RPC waits are synchronous. Keep the genuine broker LocalSet free
    // to answer them while the real mounted UI runs on its own owner thread.
    let app_thread = tokio::task::spawn_blocking(move || {
        let manager = CollaborationManager::for_route_fd_with_identity(
            socket.into_raw_fd(),
            &route.app_id,
            route.cluster_id.parse().unwrap(),
            Some(route.record_id.clone()),
            None,
            identity,
        )
        .unwrap();
        let mut app = mount_chat(&route, &certificate, manager);
        let deadline = tilo_test_env::hang_deadline();
        while !stopped.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "Chat UI proof did not finish"
            );
            app.surface_frame(&ChatSurface, &mut ui::vello::Scene::new());
            assert!(
                !matches!(
                    app.collaboration_state(),
                    ui::collaboration::CollaborationState::Offline(_)
                ),
                "certified Chat disconnected"
            );
            std::thread::yield_now();
        }
        serde_json::to_string(&automerge::AutoSerde::from(
            app.state_mut_for_test().doc.document(),
        ))
        .unwrap()
    });

    (app_thread, server, stop)
}
async fn sdk_caller(
    env: &Environment,
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    room: tilo_desktop_model::RoomId,
) -> Connection {
    let (socket, client) = std::os::unix::net::UnixStream::pair().unwrap();
    let (rpc, caller_server, caller_client) = rpc_pair(
        state.clone(),
        462,
        socket,
        client,
        None,
        directories.clone(),
    );
    let caller = rpc
        .join_channel(
            &CrdtRoute::cluster(room.to_string()).channel_key(),
            &presentation(&room.to_string(), &env.cert, &env.key),
        )
        .await
        .unwrap();
    Connection {
        handle: caller,
        server: caller_server,
        client: caller_client,
    }
}
async fn resolve_person_action(
    caller: &Connection,
    peer: &str,
    action: &str,
    deadline: std::time::Instant,
) -> (String, String, runtime_collab::PublishedInvocationBinding) {
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "actual Chat action not published: {action}"
        );
        let page = caller
            .handle
            .query_action_directory(&runtime_collab::ActionDirectoryQuery {
                peer_id: 461,
                revision: String::new(),
                cursor: Vec::new(),
                needle: String::new(),
                resource_name: String::new(),
                parent_resource: String::new(),
                max_results: 128,
                max_bytes: 128 * 1024,
                max_work: 512,
                max_depth: 16,
            })
            .await;
        if let Ok(page) = page {
            // Framework actions are mounted under the actual bound
            // record root, not the unbound live-row key.
            if !page.revision.is_empty()
                && let Some(root) = page
                    .rows
                    .iter()
                    .find(|row| row.resource_kind == <chat::Chat as tilo_resource::Resource>::KIND)
            {
                let children = caller
                    .handle
                    .query_action_directory(&runtime_collab::ActionDirectoryQuery {
                        peer_id: 461,
                        revision: String::new(),
                        cursor: Vec::new(),
                        needle: action.into(),
                        resource_name: String::new(),
                        parent_resource: root.resource.clone(),
                        max_results: 128,
                        max_bytes: 128 * 1024,
                        max_work: 4096,
                        max_depth: 16,
                    })
                    .await
                    .unwrap();
                let key = hex::encode(format!("chat.person:{peer}"));
                if let Some(person) = children
                    .rows
                    .iter()
                    .find(|row| row.resource_kind == "chat.person" && row.resource.ends_with(&key))
                    && let Ok((_, binding)) = caller
                        .handle
                        .resolve_published_action_for_invocation(
                            461,
                            &children.revision,
                            &person.resource,
                            action,
                        )
                        .await
                {
                    return (person.resource.clone(), children.revision, binding);
                }
            }
        }
        tokio::task::yield_now().await;
    }
}
async fn invoke_person_action(
    caller: &Connection,
    peer: &str,
    action: &str,
    marker: &str,
    deadline: std::time::Instant,
) -> Result<tilo_actions::ActionOutput, String> {
    let (resource, revision, binding) = resolve_person_action(caller, peer, action, deadline).await;
    let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    let argument = (action == "send-direct-message").then_some(marker);
    caller
        .handle
        .invoke_resolved_published_action_durable(
            invocation.0.as_bytes(),
            &binding,
            &revision,
            &resource,
            action,
            argument,
            "{}",
        )
        .await
        .unwrap();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "actual Chat handler did not finish: {action}"
        );
        let observed = caller
            .handle
            .published_invocation_observation(&binding.host, invocation.0.as_bytes())
            .await
            .unwrap();
        let update: tilo_actions::InvocationUpdate =
            serde_json::from_str(&observed.result.unwrap().update_json).unwrap();
        match update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => return Ok(output),
            other if other.is_terminal() => {
                return Err(format!("actual Chat handler failed: {other:?}"));
            }
            _ => tokio::task::yield_now().await,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn registered_chat_sdk_handlers_open_send_read_the_durable_private_owner() {
    let _guard = tilo_test_env::lock();
    tokio::task::LocalSet::new()
        .run_until(async {
            let SdkContext {
                env,
                owner,
                room,
                path,
                authority,
                peer,
                public_before,
                state,
                directories,
            } = sdk_context();
            let (app_thread, server, stop) = mount_sdk_chat(&env, &state, &directories);
            let mut caller = sdk_caller(&env, &state, &directories, room).await;
            let marker = "sdk-to-private-owner-body-canary";
            let deadline = tilo_test_env::hang_deadline();
            for action in [
                "open-direct-conversation",
                "send-direct-message",
                "read-direct-conversation",
            ] {
                let output = invoke_person_action(&caller, &peer, action, marker, deadline)
                    .await
                    .unwrap();
                if action == "read-direct-conversation" {
                    let value = output
                        .value
                        .expect("registered read handler returns a value");
                    assert_eq!(value["peer"], peer);
                    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
                    assert_eq!(value["messages"][0]["text"], marker);
                    assert_eq!(
                        value["messages"][0]["author"],
                        hex::encode(env.cert.user_public_key)
                    );
                }
            }
            let saved = owner
                .read_direct_conversation(
                    room,
                    &peer,
                    &authority,
                    tilo_joined_room::DirectConversationPage::default(),
                    std::time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(saved.messages.len(), 1);
            let saved_message = saved.messages.first().expect("one saved message");
            assert_eq!(saved_message.text, marker);
            assert_eq!(saved_message.sender, hex::encode(env.cert.user_public_key));
            assert_eq!(
                public_owner_bytes(&owner, room),
                public_before,
                "SDK private operations must leave public CRDT and publication bytes unchanged"
            );
            for bytes in public_owner_bytes(&owner, room) {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                );
            }
            drop(stop);
            let public_chat = app_thread.await.unwrap();
            assert!(
                !public_chat.contains(marker),
                "the mounted Chat SDK must not write private bodies into its public record"
            );
            server.abort();
            let _ = server.await;
            state.stop();
            caller.server.abort();
            caller.client.abort();
            let _ = (&mut caller.server).await;
            let _ = (&mut caller.client).await;
            drop(caller);
            drop(state);
            drop(owner);
            let reopened = tilo_joined_room::open_file(&path).unwrap();
            let persisted = reopened
                .read_direct_conversation(
                    room,
                    &peer,
                    &authority,
                    tilo_joined_room::DirectConversationPage::default(),
                    std::time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(
                persisted.messages, saved.messages,
                "success crosses the real OwnerStore barrier"
            );
        })
        .await;
}
