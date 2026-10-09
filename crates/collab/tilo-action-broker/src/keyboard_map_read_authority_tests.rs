// Actual Settings worker, private inherited route and certified shell ActionHost.
// Capture only the production bridge's static direction label, never RPC payloads.
struct KeyboardReadBridgeProbe(Arc<Mutex<Vec<String>>>);
impl tracing::Subscriber for KeyboardReadBridgeProbe {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::ERROR
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Direction<'a>(&'a Arc<Mutex<Vec<String>>>);
        impl tracing::field::Visit for Direction<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "direction" {
                    let mut directions = self.0.lock().unwrap();
                    if directions.len() < 16 {
                        directions.push(format!("{value:?}").chars().take(96).collect());
                    }
                }
            }
        }
        event.record(&mut Direction(&self.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test(flavor = "current_thread")]
async fn keyboard_map_read_retains_certified_owner_and_rejects_stale_worker_witness() {
    use std::{cell::RefCell, os::fd::IntoRawFd, os::unix::fs::PermissionsExt as _};
    use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
    use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
    use ui::portable_storage::{PortableStorage, StorageKey};
    use ui::record::{RecordCheckout, RecordId, RecordOwners};
    tokio::task::LocalSet::new()
        .run_until(async {
            let directions = Arc::new(Mutex::new(Vec::new()));
            tracing::subscriber::set_global_default(KeyboardReadBridgeProbe(directions.clone())).unwrap();
            let env = Environment::with_owner_initializer(|path| {
                crate::test_owner::SessionOwner::initialize_with_documents(path, |room| {
                    let definition = settings::definition();
                    let record = RecordId::for_tab(1);
                    let mut document = automerge::AutoCommit::new();
                    assert!(ui::record::init_scoped(&mut document, &record, &definition.record_kind, room, Some(room)));
                    Ok(vec![tilo_peer_state::SeedDocument {
                        app_id: definition.app_id.into(), record_id: record.as_str().into(),
                        kind: definition.record_kind.as_str().into(), bytes: document.save(),
                    }])
                })
            });
            tokio::task::spawn_blocking(move || {
            let mut env = env;
            let private = env.root.path().canonicalize().unwrap();
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
            env.route.app_id = "so.tilo.settings".into();
            env.route.record_id = RecordId::for_tab(1).as_str().to_owned();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let broker = crate::collab_broker::spawn_native_peer_with_services(
                private.join("keyboard-worker.sock"),
                env.membership.authority(),
                env.publisher.clone(),
                crate::peer_actions::NativePeerBrokerConfig {
                    journal_root: private.join("keyboard-journal"),
                    certificate_authority: env.authority.clone(),
                },
                env.membership.services(),
            )
            .unwrap();
            let (socket, authorize) = broker
                .connect_local_worker_with_authorizer(env.route.clone(), &env.cert)
                .unwrap();
            let manager = CollaborationManager::for_route_fd_with_identity(
                socket.into_raw_fd(),
                &env.route.app_id,
                env.route.cluster_id.parse().unwrap(),
                Some(env.route.record_id.clone()),
                None,
                CertifiedEndpointIdentity::new(
                    presentation(&env.route.cluster_id, &env.cert, &env.key),
                    env.key.clone(),
                ),
            )
            .unwrap();
            let definition = settings::definition();
            let record = RecordId::parse(&env.route.record_id).unwrap();
            let mut initial = automerge::AutoCommit::new();
            assert!(ui::record::init_scoped(&mut initial, &record, &definition.record_kind, &env.route.cluster_id, Some(&env.route.cluster_id)));
            let cache = PortableStorage::new("keyboard-read-authority", "device").unwrap();
            cache
                .hydrate(
                    StorageKey::Record {
                        package: definition.app_id.into(),
                        workspace: env.route.cluster_id.clone(),
                        record: record.as_str().into(),
                    },
                    initial.save(),
                    Some(env.cert.device_public_key.to_vec()),
                )
                .unwrap();
            let mut owners = RecordOwners::portable(cache.clone());
            let records = owners
                .open(definition.app_id, &env.route.cluster_id)
                .unwrap();
            let views = Rc::new(
                ui::application_view::InstanceViewStore::portable(cache, &env.route.cluster_id)
                    .unwrap(),
            );
            let target = LaunchTarget::Record {
                checkout: RecordCheckout::new(record.clone(), definition.record_kind.clone()),
                reveal: None,
            };
            let principal = principal_id_for_root(env.cert.user_public_key);
            let permissions = Rc::new(RefCell::new(
                ui::permissions::PermissionContext::permissive(),
            ));
            let context = HostContext::prepare(
                definition,
                HostContextInputs {
                    records: records.clone(),
                    views,
                    device_id: "device".into(),
                    instance_id: 1,
                    permissions,
                    author: ui::permissions::UserId(principal.clone()),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &target,
            )
            .unwrap();
            let component = definition.instantiate(context, target, RestoreInput::None).unwrap();
            let mut app = ui::App::from_component_with_collaboration(component,
                ui::application::ComponentRunnerInputs { theme: ui::Theme::dark(), text_renderer: ui::TextRenderer::new() }, manager).unwrap();
            struct Surface;
            impl ui::app::AppSurface for Surface {
                fn scale_factor(&self) -> f64 {
                    1.0
                }
                fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
                    ui::winit::dpi::PhysicalSize::new(1024, 768)
                }
                fn request_redraw(&self) {}
                fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
            }
            let mut scene = ui::vello::Scene::new();
            let deadline = tilo_test_env::hang_deadline();
            let (surface, action) = loop {
                scene.reset();
                app.surface_frame(&Surface, &mut scene);
                match app.collaboration_state() {
                    ui::collaboration::CollaborationState::Ready
                    | ui::collaboration::CollaborationState::Connecting => {},
                    ui::collaboration::CollaborationState::Offline(reason) => panic!("actual Settings connection failed: {reason}; publication={:?}; bridge={:?}", app.document_sync_status(), directions.lock().unwrap()),
                }
                if let Some(found) =
                    broker
                        .published_action_surfaces()
                        .into_iter()
                        .find_map(|surface| {
                            surface
                                .actions
                                .iter()
                                .find(|action| action.action == "inspect-keyboard-map")
                                .cloned()
                                .map(|action| (surface, action))
                        })
                {
                    break found;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual Settings worker must publish keyboard Read: connection={:?}, controls={:?}, resources={:?}", app.collaboration_state(), app.control_action_coverage(), app.collect_resources().iter().map(|resource| resource.id.as_str().to_owned()).collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let before = records
                .read(&record, |doc| doc.clone().get_heads())
                .unwrap();
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let facade = broker.app_action_broker().unwrap();
            let host = facade
                .certified_local_action_host(
                    &caller,
                    &presentation(&env.route.cluster_id, &env.cert, &env.key),
                )
                .unwrap();
            let request = tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: surface.peer_id.to_string(),
                    address: tilo_actions::ActionAddress::new(action.resource, action.action),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                fresh_binding: None,
                on_behalf_of: None,
                wait_ms: None,
            };
            tokio::runtime::Handle::current().block_on(local_host_answer(host.invoke(&caller, request.clone())))
                .unwrap();
            let wire_id = facade.state().remote_job(&caller, &request.invocation).unwrap();
            let delivered = ui::invocation_scope::PublishedInvocationRequest {
                wire_id, invocation: request.invocation, address: request.target.address.clone(), argument: None,
                actor: principal.clone(), participant: Some(tilo_actions::ParticipantId::for_execution(caller.execution_id().unwrap())),
            };
            let lease = authorize(&delivered).unwrap();
            assert_eq!(delivered.actor, principal);
            assert_eq!(delivered.invocation, request.invocation);
            let mut missing = delivered.clone();
            missing.invocation = tilo_actions::InvocationId::new();
            assert!(
                authorize(&missing).is_err(),
                "an unretained original cannot borrow worker identity"
            );
            let mut foreign = delivered.clone();
            foreign.actor = "unrelated-same-role-collaborator".into();
            assert!(
                authorize(&foreign).is_err(),
                "caller labels cannot replace broker custody"
            );
            lease.check().unwrap();
            let (replacement, replacement_authorize) = broker
                .connect_local_worker_with_authorizer(env.route.clone(), &env.cert).unwrap();
            assert!(replacement_authorize(&delivered).is_err(), "same app/certificate/route on another private registration cannot borrow original custody");
            lease.check().unwrap();
            let output = loop {
                scene.reset();
                app.surface_frame(&Surface, &mut scene);
                match app.collaboration_state() {
                    ui::collaboration::CollaborationState::Ready
                    | ui::collaboration::CollaborationState::Connecting => {},
                    ui::collaboration::CollaborationState::Offline(reason) => panic!("actual Settings connection failed: {reason}; publication={:?}; bridge={:?}", app.document_sync_status(), directions.lock().unwrap()),
                }
                if let Some(state) = host.observe(&caller, &request.invocation)
                    && state.is_terminal()
                {
                    break state.update;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "certified Settings Read must complete"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let tilo_actions::InvocationUpdate::Succeeded { invocation, output } = output else {
                panic!("certified owner Read failed: {output:?}");
            };
            assert_eq!(invocation, request.invocation);
            let value = output.value.unwrap();
            assert_eq!(value["page_visible"], false);
            assert_eq!(value["pressed_physical_keys"], serde_json::json!([]));
            assert_eq!(
                records
                    .read(&record, |doc| doc.clone().get_heads())
                    .unwrap(),
                before
            );
            assert!(authorize(&delivered).is_err(), "completed Read cannot be re-entered with its retired pending original");
            *env.authority.certificate.lock().unwrap() = None;
            assert!(lease.check().is_err(), "revocation retires the actual original authority");
            assert!(authorize(&delivered).is_err(), "revoked original cannot re-enter the read handler");
            assert!(tokio::runtime::Handle::current().block_on(local_host_answer(host.invoke(&caller, request))).is_err(), "revoked owner cannot invoke the Read again");
            drop(replacement);
            drop(app);
            drop(broker);
            }).await.unwrap();
        })
        .await;
}
