// The actual Store factory and certified socket must disclose operations below
// its readable root, not merely register an empty directory capability.
fn store_definition() -> &'static ui::application::ApplicationDefinition {
    app_framework::framework::definition::<store::StoreShell>()
}

fn store_genesis(room: &str) -> Vec<tilo_peer_state::SeedDocument> {
    let definition = store_definition();
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

fn mount_store(env: &Environment, manager: ui::collaboration::CollaborationManager) -> ui::App {
    use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
    use ui::portable_storage::{PortableStorage, StorageKey};
    use ui::record::{RecordCheckout, RecordId, RecordOwners};
    let definition = store_definition();
    let record = RecordId::parse(&env.route.record_id).unwrap();
    let mut initial = automerge::AutoCommit::new();
    assert!(ui::record::init_scoped(
        &mut initial,
        &record,
        &definition.record_kind,
        &env.route.cluster_id,
        Some(&env.route.cluster_id)
    ));
    let cache = PortableStorage::new("store-directory-proof", "device").unwrap();
    cache
        .hydrate(
            StorageKey::Record {
                package: definition.app_id().into(),
                workspace: env.route.cluster_id.clone(),
                record: record.as_str().into(),
            },
            initial.save(),
            Some(env.cert.device_public_key.to_vec()),
        )
        .unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let records = owners
        .open(definition.app_id(), &env.route.cluster_id)
        .unwrap();
    let views = Rc::new(
        ui::application_view::InstanceViewStore::portable(cache, &env.route.cluster_id).unwrap(),
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
            permissions: Rc::new(std::cell::RefCell::new(
                ui::permissions::PermissionContext::permissive(),
            )),
            author: ui::permissions::UserId(principal_id_for_root(env.cert.user_public_key)),
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

struct StoreDirectorySurface;
impl ui::app::AppSurface for StoreDirectorySurface {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
        ui::winit::dpi::PhysicalSize::new(1024, 768)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
}

fn pump_store(app: &mut ui::App) {
    app.surface_frame(&StoreDirectorySurface, &mut ui::vello::Scene::new());
    assert!(
        !matches!(
            app.collaboration_state(),
            ui::collaboration::CollaborationState::Offline(_)
        ),
        "actual Store socket disconnected"
    );
}

fn store_directory_page(
    app: &mut ui::App,
    broker: &crate::collab_broker::BrokerHandle,
    env: &Environment,
    query: runtime_collab::ActionDirectoryQuery,
) -> Result<runtime_collab::ActionDirectoryPage, String> {
    let facade = broker.app_action_broker().unwrap();
    let mut reply = facade
        .action_directory_request_queue()
        .try_query(
            BrokerActor::person(principal_id_for_root(env.cert.user_public_key)),
            &env.route.cluster_id,
            query,
        )
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        pump_store(app);
        match reply.try_recv() {
            Ok(Ok(ActionDirectoryBridgeResponse::Page(page))) => return Ok(page),
            Ok(other) => return Err(format!("actual Store directory reply refused: {other:?}")),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Err("Store directory reply abandoned".into());
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Store directory never answered"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn store_query(peer_id: u64, parent: &str) -> runtime_collab::ActionDirectoryQuery {
    runtime_collab::ActionDirectoryQuery {
        peer_id,
        revision: String::new(),
        cursor: Vec::new(),
        needle: String::new(),
        parent_resource: parent.into(),
        max_results: 128,
        max_bytes: 128 * 1024,
        max_work: 512,
        max_depth: 16,
        resource_name: String::new(),
    }
}

fn store_directory_children(
    app: &mut ui::App,
    broker: &crate::collab_broker::BrokerHandle,
    env: &Environment,
    peer_id: u64,
    parent: &str,
) -> Result<Vec<runtime_collab::ActionDirectoryRow>, String> {
    let mut query = store_query(peer_id, parent);
    let mut rows = Vec::new();
    let mut cursors = std::collections::HashSet::new();
    for _ in 0..16 {
        let page = store_directory_page(app, broker, env, query.clone())?;
        assert!(
            !page.stale_cursor,
            "actual Store continuation must remain current"
        );
        assert!(
            !page.revision.is_empty(),
            "refused snapshot is not an empty catalogue"
        );
        assert!(query.revision.is_empty() || query.revision == page.revision);
        rows.extend(page.rows);
        assert!(rows.len() <= 256, "fixture must retain bounded Store rows");
        if page.exhausted {
            return Ok(rows);
        }
        assert!(
            !page.next_cursor.is_empty(),
            "partial page must carry actual continuation"
        );
        assert!(
            cursors.insert(page.next_cursor.clone()),
            "a one-use cursor cannot repeat"
        );
        query.revision = page.revision;
        query.cursor = page.next_cursor;
    }
    Err("actual Store directory did not exhaust within bounded fixture pages".into())
}

fn prove_store_directory(mut env: Environment) {
    use std::{os::fd::IntoRawFd, os::unix::fs::PermissionsExt as _};
    use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
    let private = env.root.path().canonicalize().unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.route.app_id = store_definition().app_id().into();
    env.route.record_id = ui::record::RecordId::for_tab(1).as_str().into();
    env.authority = CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
    let broker = crate::collab_broker::spawn_native_peer_with_services(
        private.join("store-worker.sock"),
        env.membership.authority(),
        env.publisher.clone(),
        crate::peer_actions::NativePeerBrokerConfig {
            journal_root: private.join("store-journal"),
            certificate_authority: env.authority.clone(),
        },
        env.membership.services(),
    )
    .unwrap();
    let socket = broker
        .connect_local_worker(env.route.clone(), &env.cert)
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
    let mut app = mount_store(&env, manager);
    let deadline = tilo_test_env::hang_deadline();
    let root = loop {
        pump_store(&mut app);
        let roots = broker.app_action_broker().unwrap().action_directory_roots();
        if let Some(root) = roots.into_iter().find(|root| {
            root.app_id == env.route.app_id
                && root.cluster_id == env.route.cluster_id
                && root.record_id == env.route.record_id
        }) {
            break root;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual Store directory was never registered"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    let page = store_directory_page(&mut app, &broker, &env, store_query(root.peer_id, ""))
        .expect("actual Store root page");
    let container = page
        .rows
        .iter()
        .find(|row| row.resource_kind == "store.catalog" && row.action.is_none())
        .expect("actual readable Store root");
    assert!(container.has_children);
    // Publishing the unchanged native View/Read surface must not turn the
    // absence of process-local checkpoints into a sync failure every frame.
    // That would retire this revision (and every bounded continuation) before
    // the caller can use it, despite no resource or contract having changed.
    let revision = page.revision.clone();
    for _ in 0..3 {
        pump_store(&mut app);
        let current = store_directory_page(&mut app, &broker, &env, store_query(root.peer_id, ""))
            .expect("unchanged Store publication");
        assert_eq!(current.revision, revision);
    }
    let children =
        store_directory_children(&mut app, &broker, &env, root.peer_id, &container.resource)
            .expect("actual Store child pages");
    for operation in ["inspect-catalogue", "refresh"] {
        assert!(
            children
                .iter()
                .any(|row| row.resource_kind == "store.catalog"
                    && row.action.as_deref() == Some(operation)),
            "missing actual Store child {operation}; observed {} rows",
            children.len()
        );
    }
    drop(app);
    drop(broker);
}

#[tokio::test(flavor = "current_thread")]
async fn store_native_certified_directory_discloses_children_after_actual_history() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::with_owner_initializer(|path| {
                crate::test_owner::SessionOwner::initialize_with_documents(path, |room| {
                    Ok(store_genesis(room))
                })
            });
            let ProvisionedEnvironment {
                environment: env,
                _identity_env: _identity,
            } = env;
            tokio::task::spawn_blocking(move || prove_store_directory(env))
                .await
                .unwrap();
        })
        .await;
}
