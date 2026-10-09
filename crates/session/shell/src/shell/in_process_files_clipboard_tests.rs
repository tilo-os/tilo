// Two production Files components, actual native admission and host service.
// Qualifies publication, Paste and compensation through the semantic action
// door against actual file bytes. Rendering is qualified separately.
mod files_clipboard_tests {
    use super::*;
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Path, PathBuf};
    use tilo_actions::remote::{Pending, PendingPoll};

    struct RetiredOwner {
        tree: crate::shell::state::WeakWorkspaceTreeHandle,
        runtime: crate::shell::native_workspace_owner::NativeStateWitness,
        broker: std::sync::Weak<tilo_action_broker::collab_broker_capnp::CapnpBrokerState>,
    }

    thread_local! {
        static RETIRED: std::cell::RefCell<Option<RetiredOwner>> = const { std::cell::RefCell::new(None) };
    }

    fn restart_lease_diagnostics(profile: &Path) {
        RETIRED.with(|probe| {
            if let Some(owner) = &*probe.borrow() {
                eprintln!(
                    "retired native owner: tree refs={}, runtime refs={}, broker refs={}",
                    owner.tree.strong_count(),
                    owner.runtime.retained_owners(),
                    owner.broker.strong_count()
                );
            }
        });
        eprintln!(
            "peer sync configured: {}",
            crate::shell::network::collab_host_configured()
        );
        let sentinel = std::fs::metadata(profile.join("workspace.json.writer-lock"));
        if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
            for entry in entries.flatten() {
                if sentinel.as_ref().is_ok_and(|wanted| {
                    std::fs::metadata(entry.path()).is_ok_and(|found| {
                        found.dev() == wanted.dev() && found.ino() == wanted.ino()
                    })
                }) {
                    let info = std::fs::read_to_string(
                        Path::new("/proc/self/fdinfo").join(entry.file_name()),
                    );
                    eprintln!("native lease fd {:?}: {info:?}", entry.file_name());
                }
            }
        }
        if let Ok(metadata) = sentinel {
            let inode = format!(":{}", metadata.ino());
            match std::fs::read_to_string("/proc/locks") {
                Ok(locks) => {
                    for line in locks
                        .lines()
                        .filter(|line| line.split_whitespace().any(|field| field.ends_with(&inode)))
                    {
                        eprintln!("native lease kernel lock: {line}");
                    }
                }
                Err(error) => eprintln!("cannot inspect kernel locks: {error}"),
            }
        }
        if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
            for entry in entries.flatten() {
                eprintln!(
                    "remaining thread {:?}: {:?}",
                    entry.file_name(),
                    std::fs::read_to_string(entry.path().join("comm"))
                );
            }
        }
    }

    const APP: &str = "so.tilo.file-explorer";
    const TABS: [u64; 2] = [601, 602];

    struct InputSurface;

    impl ui::app::AppSurface for InputSurface {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> ui::winit::dpi::PhysicalSize<u32> {
            ui::winit::dpi::PhysicalSize::new(900, 600)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _: ui::winit::window::CursorIcon) {}
    }

    type AgentRequest = (
        tilo_actions::remote::RemoteActionMessage,
        tilo_actions::remote::Completer<Result<tilo_actions::remote::RemoteActionMessage, String>>,
    );

    struct AgentDoor {
        send: tokio::sync::mpsc::UnboundedSender<Option<AgentRequest>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl AgentDoor {
        fn open(session: &crate::runtime_host::Session, agent: &str) -> Self {
            use ed25519_dalek::Signer as _;
            let (key, certificate) =
                tilo_identity_host::broker::trusted_current_device(&session.room)
                    .expect("the actual selected certified device delegates the agent");
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: session.room.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        &session.room,
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            let endpoint = session.broker.capnp_socket().to_path_buf();
            let room = session.room.clone();
            let agent = agent.to_owned();
            let (send, mut receive) =
                tokio::sync::mpsc::unbounded_channel::<Option<AgentRequest>>();
            let thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(tokio::task::LocalSet::new().run_until(async move {
                    let descriptor =
                        runtime_portal::agent_transport::connect_endpoint(&endpoint).unwrap();
                    let connection = runtime_portal::CapnpClient::from_connected_fd(
                        descriptor,
                        capnp_rpc::rpc_twoparty_capnp::Side::Client,
                    )
                    .unwrap();
                    let client = runtime_collab::CollabClient::from_raw(connection.bootstrap());
                    let channel = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        client.join_channel_as_kind(&room, &presentation, &agent, "agent", ""),
                    )
                    .await
                    .expect("agent admission must answer within the test deadline")
                    .expect(
                        "the signed current device and live workspace membership admit this agent",
                    );
                    while let Some(Some((message, complete))) = receive.recv().await {
                        let bytes = tilo_actions::remote::RemoteActionFrame::new(message)
                            .encode()
                            .unwrap();
                        let result = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            channel.remote_action(&bytes),
                        )
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result.map_err(|error| error.to_string()))
                        .and_then(|bytes| {
                            tilo_actions::remote::RemoteActionFrame::decode(&bytes)
                                .map(|frame| frame.message)
                                .map_err(|error| error.to_string())
                        });
                        complete.complete(result);
                    }
                }));
            });
            Self {
                send,
                thread: Some(thread),
            }
        }

        fn request(
            &self,
            message: tilo_actions::remote::RemoteActionMessage,
        ) -> Pending<Result<tilo_actions::remote::RemoteActionMessage, String>> {
            let (complete, pending) = tilo_actions::remote::pending();
            self.send
                .send(Some((message, complete)))
                .expect("the actual agent channel is alive");
            pending
        }
    }

    impl Drop for AgentDoor {
        fn drop(&mut self) {
            let _ = self.send.send(None);
            if let Some(thread) = self.thread.take()
                && let Err(error) = thread.join()
                && !std::thread::panicking()
            {
                std::panic::resume_unwind(error);
            }
        }
    }

    struct Fixture {
        shown: [Shown; 2],
        apps: InProcessApps,
        host: crate::shell::certified_action_session::CertifiedActionSession,
        broker: crate::shell::collab_broker::AppActionBroker,
        providers: [String; 2],
        folders: [PathBuf; 2],
        source_grants: Vec<String>,
        session: crate::runtime_host::Session,
    }

    impl Fixture {
        fn new(profile: &Path) -> Self {
            Self::open(profile, true)
        }

        fn resume(profile: &Path) -> Self {
            Self::open(profile, false)
        }

        fn open(profile: &Path, prepare: bool) -> Self {
            Self::open_in(
                profile,
                prepare,
                [profile.join("source"), profile.join("receiving")],
            )
        }

        fn start_session(profile: &Path, restarting: bool) -> crate::runtime_host::Session {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(5))
                .expect("bounded restart deadline");
            loop {
                let mut stage = String::new();
                match crate::runtime_host::Session::start_reporting(profile, &mut |step| {
                    stage = step.into();
                }) {
                    Ok(session) => return session,
                    // Dropping a same-process session asks asynchronous hosts
                    // to stop. Wait for that actual retirement before reopening;
                    // a real process-kill journey releases its locks at exit.
                    Err(error)
                        if restarting
                            && matches!(
                                error.to_string().as_str(),
                                "Busy" | "file lease is busy"
                            )
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => {
                        restart_lease_diagnostics(profile);
                        panic!("actual session start failed at {stage}: {error}");
                    }
                }
            }
        }

        fn open_in(profile: &Path, prepare: bool, folders: [PathBuf; 2]) -> Self {
            std::fs::create_dir_all(profile).unwrap();
            if prepare {
                for folder in &folders {
                    std::fs::create_dir(folder).unwrap();
                }
                std::fs::write(folders[0].join("report"), b"original source").unwrap();
                std::fs::write(folders[1].join("other"), b"receiving file").unwrap();
            } else {
                assert!(folders.iter().all(|folder| folder.is_dir()));
            }
            let session = Self::start_session(profile, !prepare);
            let source_grants = {
                let tree = session.workspace_tree();
                let mut tree = tree.write();
                let workspace = &mut tree.workspaces[0];
                let grant = |path: &PathBuf| Capability::Filesystem {
                    path: path.clone(),
                    mode: FsMode::ReadWrite,
                    scope: GRANT_SESSION,
                };
                let source_grants = workspace.grant_app(APP, &grant(&folders[0]));
                workspace.grant_app(APP, &grant(&folders[1]));
                source_grants
            };
            let collab = session.broker.capnp_socket().to_path_buf();
            let identity = session.identity_socket().to_path_buf();
            let mut apps =
                mounted_apps(profile, &session.actor).reaching_broker(Rc::new(move |app, room| {
                    session_connector(collab.clone(), identity.clone(), app, room)
                }));
            let mut shown = std::array::from_fn(|index| {
                Shown::mount(
                    &mut apps,
                    file_explorer::definition(),
                    &session.room,
                    TABS[index],
                    |context| {
                        context.lend(file_explorer::StartupInputs {
                            initial_dir: folders[index].clone(),
                            workspace_roots: folders.to_vec(),
                            record_roots: Vec::new(),
                            remote_accounts: Vec::new(),
                            trusted_humans:
                                file_explorer::identity_directory::TrustedHumanDirectory::default(),
                        });
                        context.lend(
                            file_explorer::files_app::HostedFileServices::deferred_document_open(),
                        );
                    },
                )
            });
            let broker = session.broker.app_action_broker().unwrap();
            let provider = |tab| {
                broker
                    .state()
                    .action_directory_root_page(None, 16)
                    .roots
                    .into_iter()
                    .find(|root| {
                        root.app_id == APP && root.record_id == RecordId::for_tab(tab).to_string()
                    })
                    .map(|root| root.peer_id.to_string())
            };
            let [first, second] = &mut shown;
            until(
                &mut [first, second],
                "two actual Files windows publish",
                || TABS.iter().all(|tab| provider(*tab).is_some()),
            );
            Self {
                shown,
                apps,
                host: crate::shell::certified_action_session::CertifiedActionSession::open(
                    broker.clone(),
                    &session.room,
                    Some(&session.actor),
                )
                .expect("the actual selected device supplies certified desktop action custody"),
                broker: broker.clone(),
                providers: TABS.map(|tab| provider(tab).unwrap()),
                folders,
                source_grants,
                session,
            }
        }

        fn wait<T>(&mut self, mut pending: Pending<T>) -> T {
            let mut result = None;
            let [first, second] = &mut self.shown;
            until(
                &mut [first, second],
                "actual native broker reply",
                || match pending.poll_ready() {
                    PendingPoll::Ready(value) => {
                        result = Some(value);
                        true
                    }
                    PendingPoll::Waiting => false,
                    PendingPoll::Abandoned => panic!("native broker abandoned the request"),
                },
            );
            result.unwrap()
        }

        fn invoke(&mut self, index: usize, action: &str) -> tilo_actions::InvocationUpdate {
            self.invoke_with(index, action, serde_json::Map::default())
        }

        fn invoke_with(
            &mut self,
            index: usize,
            action: &str,
            arguments: serde_json::Map<String, serde_json::Value>,
        ) -> tilo_actions::InvocationUpdate {
            self.invoke_at(index, action, arguments, None)
        }

        fn invoke_at(
            &mut self,
            index: usize,
            action: &str,
            arguments: serde_json::Map<String, serde_json::Value>,
            entry_label: Option<&str>,
        ) -> tilo_actions::InvocationUpdate {
            let (job, _) = self.start_invoke_at(index, action, arguments, entry_label);
            self.settle_job(job)
        }

        fn start_invoke_at(
            &mut self,
            index: usize,
            action: &str,
            arguments: serde_json::Map<String, serde_json::Value>,
            entry_label: Option<&str>,
        ) -> (u64, tilo_actions::InvocationId) {
            let page = self.host.query(tilo_actions::remote::CatalogueQuery {
                provider: Some(self.providers[index].clone()),
                needle: action.into(),
                ..Default::default()
            });
            let page = self.wait(page).unwrap();
            let listed = format!("{:?}", page.rows);
            let address = page
                .rows
                .into_iter()
                .filter(|row| {
                    entry_label.is_none_or(|label| {
                        row.resource.kind == "files.entry" && row.resource.label == label
                    })
                })
                .filter_map(|row| row.action)
                .find(|row| row.address.action == action)
                .unwrap_or_else(|| panic!("actual Files window does not offer {action}: {listed}"))
                .address;
            let invocation = tilo_actions::InvocationId::new();
            let pending = self.host.invoke_job(tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: self.providers[index].clone(),
                    address,
                },
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
            });
            let (job, receipt) = self.wait(pending).unwrap();
            assert_eq!(receipt.invocation, invocation);
            (job, invocation)
        }

        fn settle_job(&mut self, job: u64) -> tilo_actions::InvocationUpdate {
            let mut update = None;
            let [first, second] = &mut self.shown;
            until(&mut [first, second], "actual Files job settles", || {
                update = self.broker.state().invocation_status(job);
                update
                    .as_ref()
                    .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            });
            update.unwrap()
        }

        fn agent_invoke(
            &mut self,
            agent: &AgentDoor,
            index: usize,
            action: &str,
        ) -> tilo_actions::InvocationUpdate {
            use tilo_actions::remote::{
                InvokeArgument, InvokeRequest, RemoteActionMessage as Message, RemoteActionTarget,
            };
            let pending = agent.request(Message::CatalogueRequest {
                correlation: 1,
                query: tilo_actions::remote::CatalogueQuery {
                    provider: Some(self.providers[index].clone()),
                    needle: action.into(),
                    ..Default::default()
                },
            });
            let Message::CatalogueResponse { result, .. } = self.wait(pending).unwrap() else {
                panic!("the actual agent must receive a catalogue response");
            };
            let address = result
                .unwrap()
                .rows
                .into_iter()
                .filter_map(|row| row.action)
                .find(|row| row.address.action == action)
                .expect("the agent discovers the actual action")
                .address;
            let invocation = tilo_actions::InvocationId::new();
            let pending = agent.request(Message::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: RemoteActionTarget {
                    provider: self.providers[index].clone(),
                    address,
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
            }));
            let Message::Receipt(result) = self.wait(pending).unwrap() else {
                panic!("the actual agent must receive its invocation receipt");
            };
            assert_eq!(result.invocation, invocation);
            assert!(
                !matches!(
                    result.status,
                    tilo_actions::remote::ReceiptStatus::Rejected { .. }
                ),
                "actual agent admission refused: {result:?}"
            );
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(5))
                .expect("bounded test deadline");
            loop {
                let pending = agent.request(Message::Observe { invocation });
                let Message::Update { state, .. } = self.wait(pending).unwrap() else {
                    panic!("the actual agent must observe its own invocation");
                };
                let state = state.expect("the authenticated agent still owns its invocation");
                if state.is_terminal() {
                    return state.update;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "actual agent clipboard invocation did not settle: {state:?}"
                );
            }
        }

        fn succeeds(&mut self, index: usize, action: &str) -> serde_json::Value {
            let update = self.invoke(index, action);
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                panic!("actual Files {action} failed: {update:?}");
            };
            output.value.unwrap_or(serde_json::Value::Null)
        }

        fn capture(
            &mut self,
            renderer: &mut ui::offscreen::OffscreenRenderer,
            theme: &ui::theme::Theme,
            name: &str,
        ) {
            let mut scene = vello::Scene::new();
            for (index, shown) in self.shown.iter_mut().enumerate() {
                let mut nodes = std::collections::HashMap::new();
                let node = shown.tile.build_layout(&mut shown.taffy, theme, &mut nodes);
                shown
                    .taffy
                    .compute_layout(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(900.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                    )
                    .unwrap();
                shown.tile.apply_layout(
                    &shown.taffy,
                    node,
                    vello::kurbo::Point::new(if index == 0 { 0.0 } else { 900.0 }, 0.0),
                );
                shown.tile.draw(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut shown.text,
                    theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
            let output = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target/files-clipboard-evidence")
                .join(format!("{name}.png"));
            renderer.render_png(&scene, theme.region(ui::surface::Region::Content), output);
        }

        fn wait_for_paste_history(&mut self, undone: Option<bool>, failure: &str) {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(5))
                .expect("bounded history deadline");
            loop {
                let history = self.succeeds(1, "history");
                if history["versions"].as_array().unwrap().iter().any(|entry| {
                    entry["what"]["action"]["action"] == "record-clipboard-transfer"
                        && undone.is_none_or(|undone| entry["undone"] == undone)
                }) {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "{failure}: {history}");
            }
        }

        fn command_key(&mut self, index: usize, character: &str) {
            self.command_key_with_modifiers(
                index,
                character,
                ui::winit::keyboard::ModifiersState::CONTROL,
            );
        }

        fn command_key_with_modifiers(
            &mut self,
            index: usize,
            character: &str,
            modifiers: ui::winit::keyboard::ModifiersState,
        ) {
            use ui::winit::event::{DeviceId, ElementState, KeyEvent, RawKeyEvent, WindowEvent};
            use ui::winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
            let embedded = self.shown[index]
                .tile
                .as_any_mut()
                .unwrap()
                .downcast_mut::<ui::app::EmbeddedApp>()
                .unwrap();
            let app = embedded.app_mut();
            app.surface_input(&InputSurface, WindowEvent::Focused(true), false);
            app.set_surface_modifiers(modifiers.into());
            for state in [ElementState::Pressed, ElementState::Released] {
                let key = Key::Character(character.into());
                let event = KeyEvent::synthetic(
                    RawKeyEvent {
                        physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                        state,
                    },
                    key.clone(),
                    None,
                    KeyLocation::Standard,
                    false,
                    None,
                    key,
                );
                app.surface_input(
                    &InputSurface,
                    WindowEvent::KeyboardInput {
                        device_id: DeviceId::dummy(),
                        event,
                        is_synthetic: false,
                    },
                    false,
                );
            }
        }
    }

    #[test]
    fn two_mounted_files_windows_publish_replace_and_revoke_native_offers() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::two_mounted_files_windows_publish_replace_and_revoke_native_offers";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        assert_ne!(fixture.providers[0], fixture.providers[1]);
        assert!(fixture.succeeds(1, "clipboard-status")["offer"].is_null());
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        let first = fixture.succeeds(1, "clipboard-status")["offer"].clone();
        assert_eq!(first["count"], 1);
        assert_eq!(first["cut"], false);
        assert!(uuid::Uuid::parse_str(first["id"].as_str().unwrap()).is_ok());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        fixture.succeeds(1, "select-all");
        fixture.succeeds(1, "cut-selection");
        let second = fixture.succeeds(0, "clipboard-status")["offer"].clone();
        assert_ne!(first["id"], second["id"]);
        assert_eq!(second["count"], 1);
        assert_eq!(second["cut"], true);
        assert_eq!(
            std::fs::read(fixture.folders[1].join("other")).unwrap(),
            b"receiving file"
        );
        fixture.succeeds(0, "copy-selection");
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            for grant in &fixture.source_grants {
                assert!(tree.workspaces[0].revoke_capability(grant));
            }
            drop(tree);
        }
        assert!(
            fixture.succeeds(1, "clipboard-status")["offer"].is_null(),
            "actual source grant revocation invalidates the published offer"
        );
        assert!(matches!(
            fixture.invoke(0, "copy-selection"),
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
        drop(fixture);
    }

    #[test]
    fn mounted_files_paste_copies_exact_source_and_records_native_receipt() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_paste_copies_exact_source_and_records_native_receipt";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        fixture.succeeds(1, "paste");
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        let history = fixture.succeeds(1, "history");
        let entry = history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
            .unwrap_or_else(|| panic!("native Paste's actual shared history: {history}"));
        let args = &entry["what"]["args"];
        assert_eq!(
            args["receipt.completed"], 1,
            "the production history records the receipted count: {history}"
        );
        assert_eq!(
            args["receipt.cut"], false,
            "the production history records Copy mode: {history}"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_redo_restores_exact_copy_and_cut_objects_and_refuses_replacement() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_redo_restores_exact_copy_and_cut_objects_and_refuses_replacement";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        for action in ["copy-selection", "cut-selection"] {
            let mut fixture = Fixture::new(&profile.join(action));
            fixture.succeeds(0, "select-all");
            fixture.succeeds(0, action);
            fixture.succeeds(1, "paste");
            let destination = fixture.folders[1].join("report");
            std::fs::write(&destination, b"edited pasted object").unwrap();
            let inode = std::fs::symlink_metadata(&destination).unwrap().ino();
            fixture.succeeds(1, "undo");
            assert!(!destination.exists());
            std::fs::write(&destination, b"replacement must survive").unwrap();
            let update = fixture.invoke(1, "redo");
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Failed { .. }),
                "Redo must refuse replacement: {action}: {update:?}"
            );
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"replacement must survive"
            );
            let history = fixture.succeeds(1, "history");
            let paste = history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
                .unwrap();
            assert_eq!(
                paste["undone"], true,
                "failed native Redo must remain undone: {history}"
            );
            std::fs::remove_file(&destination).unwrap();
            fixture.succeeds(1, "redo");
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"edited pasted object"
            );
            assert_eq!(
                std::fs::symlink_metadata(&destination).unwrap().ino(),
                inode,
                "Redo must restore the retained object, including later edits"
            );
            let history = fixture.succeeds(1, "history");
            let paste = history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
                .unwrap();
            // The production history API emits `undone` only when true.
            assert!(
                paste.get("undone").is_none(),
                "native Redo receipt must return the Paste to in-effect history: {history}"
            );
            fixture.succeeds(1, "undo");
            assert!(
                !destination.exists(),
                "Undo must work again after native Redo"
            );
            if action == "cut-selection" {
                assert_eq!(
                    std::fs::read(fixture.folders[0].join("report")).unwrap(),
                    b"edited pasted object"
                );
            }
            drop(fixture);
        }
    }

    #[test]
    fn mounted_files_recovers_cut_after_process_kill_before_history_receipt() {
        use std::os::unix::process::ExitStatusExt as _;
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_recovers_cut_after_process_kill_before_history_receipt";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        if std::env::var_os("TILO_CLIPBOARD_TEST_KILL_AT").is_some() {
            let mut fixture = Fixture::new(&profile);
            fixture.succeeds(0, "select-all");
            fixture.succeeds(0, "cut-selection");
            fixture.succeeds(1, "paste");
            drop(fixture);
            panic!("the production Cut phase was not reached");
        }
        for phase in ["cut-before-remove", "cut-before-publish"] {
            let root = profile.join(phase);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env("TILO_IN_PROCESS_TEST_ROOT", &root)
                .env("TILO_CLIPBOARD_TEST_KILL_AT", phase)
                .status()
                .unwrap();
            assert_eq!(
                status.signal(),
                Some(libc::SIGKILL),
                "must kill the real destructive phase: {phase}: {status}"
            );
            assert!(!root.join("source/report").exists());
            assert!(!root.join("receiving/report").exists());
            let mut fixture = Fixture::resume(&root);
            let history = fixture.succeeds(1, "history");
            assert!(
                !history["versions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"),
                "interruption must precede document receipt: {history}"
            );
            let result = fixture.succeeds(1, "recover-clipboard-transfers");
            assert_eq!(
                result["completed"], 1,
                "actual recovery must settle its interrupted receipt: {phase}: {result}"
            );
            assert_eq!(
                std::fs::read(root.join("source/report")).unwrap(),
                b"original source"
            );
            assert!(!root.join("receiving/report").exists());
            assert!(
                !std::fs::read_dir(root.join("source"))
                    .unwrap()
                    .any(|entry| entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".tilo-transfer-source-")),
                "recovery must leave no hidden source quarantine"
            );
            assert_eq!(
                fixture.succeeds(1, "recover-clipboard-transfers")["completed"],
                0,
                "recovery is idempotent"
            );
            drop(fixture);
        }
    }

    #[test]
    fn mounted_files_cancelled_cut_recovers_without_false_paste_history() {
        use crate::shell::broker_services::TestTransferPause;
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cancelled_cut_recovers_without_false_paste_history";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "cut-selection");
        let pause = TestTransferPause::new(&fixture.folders[1], "before-source-remove");
        let (job, invocation) = fixture.start_invoke_at(1, "paste", serde_json::Map::new(), None);
        {
            let [first, second] = &mut fixture.shown;
            until(
                &mut [first, second],
                "actual Cut reaches its source-removal boundary",
                || pause.entered(),
            );
        }
        assert!(!fixture.folders[0].join("report").exists());
        assert!(!fixture.folders[1].join("report").exists());
        fixture
            .host
            .cancel(&invocation)
            .expect("the actual certified caller cancels its own admitted Paste");
        pause.release();
        {
            let [first, second] = &mut fixture.shown;
            until(
                &mut [first, second],
                "actual cancelled native worker stops",
                || pause.finished(),
            );
        }
        let update = fixture.settle_job(job);
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }),
            "cancelled Paste must not report success: {update:?}"
        );
        let recovered = fixture.succeeds(1, "recover-clipboard-transfers");
        assert_eq!(recovered["completed"], 1);
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        assert!(!fixture.folders[1].join("report").exists());
        let history = fixture.succeeds(1, "history");
        drop(fixture);
        assert!(
            !history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"),
            "cancelled unpublished Paste has no successful transfer history: {history}"
        );
    }

    #[test]
    fn mounted_files_partial_paste_records_only_its_published_objects() {
        use crate::shell::broker_services::TestTransferPause;
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_partial_paste_records_only_its_published_objects";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        std::fs::write(
            fixture.folders[0].join("second"),
            b"second protected source",
        )
        .unwrap();
        fixture.succeeds(0, "refresh");
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        let pause = TestTransferPause::new(&fixture.folders[1], "after-item");
        let (job, _) = fixture.start_invoke_at(1, "paste", serde_json::Map::new(), None);
        {
            let [first, second] = &mut fixture.shown;
            until(
                &mut [first, second],
                "one actual selected object is published",
                || pause.entered(),
            );
        }
        let receiving = fixture.folders[1].clone();
        let first = std::fs::read_dir(&receiving)
            .unwrap()
            .map(Result::unwrap)
            .map(|entry| entry.path())
            .find(|path| path.file_name().unwrap() != "other")
            .unwrap();
        let first_bytes = std::fs::read(&first).unwrap();
        assert_eq!(
            std::fs::read_dir(&receiving).unwrap().count(),
            2,
            "only one clipboard object is actually published at this phase"
        );
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            for grant in &fixture.source_grants {
                assert!(tree.workspaces[0].revoke_capability(grant));
            }
            drop(tree);
        }
        pause.release();
        let update = fixture.settle_job(job);
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Failed { .. }),
            "partial Paste must report its refusal: {update:?}"
        );
        assert_eq!(std::fs::read_dir(&receiving).unwrap().count(), 2);
        assert_eq!(std::fs::read(&first).unwrap(), first_bytes);
        assert_eq!(
            std::fs::read(fixture.folders[0].join("second")).unwrap(),
            b"second protected source"
        );
        let history = fixture.succeeds(1, "history");
        assert!(
            history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"),
            "the physically published subset must remain undoable: {history}"
        );
        fixture.succeeds(1, "undo");
        assert_eq!(std::fs::read_dir(&receiving).unwrap().count(), 1);
        assert!(!first.exists());
        drop(fixture);
    }

    fn assert_unsaved_undo_intent(fixture: &mut Fixture) {
        let owner = fixture
            .apps
            .owners
            .open(APP, &fixture.session.room)
            .unwrap();
        assert!(
            owner
                .read(
                    &RecordId::for_tab(TABS[1]),
                    ui::history_source::steps::pending_compensations,
                )
                .expect("the real receiving document must be open")
                .is_empty(),
            "this crash must precede durable Undo intent; the saved-intent branch has its own journey",
        );
    }

    fn mounted_apps(profile: &Path, author: &str) -> InProcessApps {
        let apps_root = profile.join("apps");
        if std::env::var_os("TILO_CLIPBOARD_TEST_DEFER_DOC_SAVE").is_none() {
            return InProcessApps::at(&apps_root, author).unwrap();
        }
        // Keep the real native store and its acknowledgements. Only this crash
        // child's write schedule differs: Paste is explicitly flushed before Undo.
        std::fs::create_dir_all(&apps_root).unwrap();
        let native = ui::application::NativeProfile::open(&apps_root, None).unwrap();
        InProcessApps::new(
            Rc::new(app_state::FsStore::with_debounce(
                apps_root,
                std::time::Duration::from_secs(600),
            )),
            native.device_id(),
            author,
        )
    }

    fn kill_after_saving_cross_device_undo(profile: &Path, receiving: PathBuf) -> ! {
        let mut fixture = Fixture::open_in(profile, true, [profile.join("source"), receiving]);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "cut-selection");
        fixture.succeeds(1, "paste");
        let owner = fixture
            .apps
            .owners
            .open(APP, &fixture.session.room)
            .unwrap();
        owner.try_flush().unwrap();
        let pause = crate::shell::broker_services::TestTransferPause::new(
            &fixture.folders[1],
            "compensation-after-publish",
        );
        fixture.start_invoke_at(1, "undo", serde_json::Map::default(), None);
        let [first, second] = &mut fixture.shown;
        until(
            &mut [first, second],
            "actual Undo target publication",
            || pause.entered(),
        );
        let pending = owner
            .read(
                &RecordId::for_tab(TABS[1]),
                ui::history_source::steps::pending_compensations,
            )
            .expect("the real receiving document must be open");
        assert_eq!(
            pending.len(),
            1,
            "the original Undo must still await its host receipt"
        );
        owner.try_flush().unwrap();
        // Kill only after the real owner's acknowledgement of this pending intent.
        // The worker is still held after durable publication and before removal.
        // SAFETY: this phase runs only in its isolated child; the fixed
        // signal targets that process's own PID and invokes no cleanup.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        // The live session must survive until the signal. This cleanup can only
        // execute if the kill failed, in which case the test must fail too.
        drop(fixture);
        unreachable!("the isolated test process must be killed");
    }

    #[test]
    fn mounted_files_cold_recovery_settles_a_saved_cross_device_undo() {
        use std::os::unix::{fs::MetadataExt as _, process::ExitStatusExt as _};
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cold_recovery_settles_a_saved_cross_device_undo";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        if let Some(receiving) = std::env::var_os("TILO_CLIPBOARD_TEST_CROSS_DEVICE") {
            kill_after_saving_cross_device_undo(&profile, PathBuf::from(receiving));
        }
        std::fs::create_dir_all(&profile).unwrap();
        let remote_mount = tempfile::Builder::new()
            .prefix("tilo-saved-undo-cross-device-")
            .tempdir_in("/dev/shm")
            .expect("a real second filesystem is required");
        assert_ne!(
            std::fs::metadata(&profile).unwrap().dev(),
            std::fs::metadata(remote_mount.path()).unwrap().dev(),
        );
        let root = profile.join("interrupted");
        let receiving = remote_mount.path().join("receiving");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("TILO_IN_PROCESS_TEST_ROOT", &root)
            .env("TILO_CLIPBOARD_TEST_CROSS_DEVICE", &receiving)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(
            std::fs::read(root.join("source/report")).unwrap(),
            b"original source"
        );
        let mut fixture = Fixture::open_in(&root, false, [root.join("source"), receiving.clone()]);
        assert_eq!(
            fixture.succeeds(1, "recover-clipboard-transfers")["completed"],
            1
        );
        assert!(!receiving.join("report").exists());
        assert_eq!(
            std::fs::read(root.join("source/report")).unwrap(),
            b"original source"
        );
        let history = fixture.succeeds(1, "history");
        assert!(
            history["versions"].as_array().unwrap().iter().any(
                |entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"
                    && entry["undone"] == true
            ),
            "cold recovery must settle the actual saved Undo: {history}"
        );
        let result = fixture.invoke(1, "undo");
        drop(fixture);
        assert!(
            matches!(&result, tilo_actions::InvocationUpdate::Failed { error, .. }
            if error.message.contains("nothing in effect to undo")),
            "a second Undo has no in-effect step after recovery: {result:?}"
        );
        assert_eq!(
            std::fs::read(root.join("source/report")).unwrap(),
            b"original source"
        );
        assert!(!receiving.join("report").exists());
    }

    #[test]
    fn mounted_files_cross_device_undo_recovers_after_process_kill() {
        use std::os::unix::{fs::MetadataExt as _, process::ExitStatusExt as _};
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cross_device_undo_recovers_after_process_kill";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        if let Some(receiving) = std::env::var_os("TILO_CLIPBOARD_TEST_CROSS_DEVICE") {
            let mut fixture = Fixture::open_in(
                &profile,
                true,
                [profile.join("source"), PathBuf::from(receiving)],
            );
            fixture.succeeds(0, "select-all");
            fixture.succeeds(0, "cut-selection");
            fixture.succeeds(1, "paste");
            fixture
                .apps
                .owners
                .open(APP, &fixture.session.room)
                .unwrap()
                .try_flush()
                .unwrap();
            fixture.succeeds(1, "undo");
            drop(fixture);
            panic!("the actual cross-device compensation phase was not reached");
        }
        std::fs::create_dir_all(&profile).unwrap();
        let remote_mount = tempfile::Builder::new()
            .prefix("tilo-clipboard-kill-cross-device-")
            .tempdir_in("/dev/shm")
            .expect("a real second filesystem is required");
        assert_ne!(
            std::fs::metadata(&profile).unwrap().dev(),
            std::fs::metadata(remote_mount.path()).unwrap().dev()
        );
        let root = profile.join("interrupted");
        let receiving = remote_mount.path().join("receiving");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("TILO_IN_PROCESS_TEST_ROOT", &root)
            .env("TILO_CLIPBOARD_TEST_KILL_AT", "compensation-after-publish")
            .env("TILO_CLIPBOARD_TEST_DEFER_DOC_SAVE", "1")
            .env("TILO_CLIPBOARD_TEST_CROSS_DEVICE", &receiving)
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "must kill the actual worker after its durable target publication: {status}"
        );
        assert_eq!(
            std::fs::read(root.join("source/report")).unwrap(),
            b"original source"
        );
        let mut fixture = Fixture::open_in(&root, false, [root.join("source"), receiving.clone()]);
        assert_unsaved_undo_intent(&mut fixture);
        let result = fixture.succeeds(1, "recover-clipboard-transfers");
        assert_eq!(
            result["completed"], 1,
            "the fresh receiving record resumes its actual durable move: {result}"
        );
        assert_eq!(
            std::fs::read(root.join("source/report")).unwrap(),
            b"original source"
        );
        assert!(!receiving.join("report").exists());
        assert!(std::fs::read_dir(&receiving).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tilo-clipboard-relocate")
        }));
        assert_eq!(
            fixture.succeeds(1, "recover-clipboard-transfers")["completed"],
            0
        );
        // A fresh canonical Undo can settle even if the killed process had not
        // yet flushed its document intent. No surviving worker token is reused.
        fixture.succeeds(1, "undo");
        let history = fixture.succeeds(1, "history");
        drop(fixture);
        assert!(history["versions"].as_array().unwrap().iter().any(
            |entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"
                && entry["undone"] == true
        ));
    }

    #[test]
    fn mounted_files_clipboard_undo_redo_crosses_real_filesystems() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_clipboard_undo_redo_crosses_real_filesystems";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        std::fs::create_dir_all(&profile).unwrap();
        let remote_mount = tempfile::Builder::new()
            .prefix("tilo-clipboard-cross-device-")
            .tempdir_in("/dev/shm")
            .expect("a real second filesystem is required");
        assert_ne!(
            std::fs::metadata(&profile).unwrap().dev(),
            std::fs::metadata(remote_mount.path()).unwrap().dev(),
            "the journey must actually cross filesystems"
        );
        for cut in [false, true] {
            let receiving = remote_mount.path().join(if cut { "cut" } else { "copy" });
            let own_profile = profile.join(if cut { "cut" } else { "copy" });
            let mut fixture =
                Fixture::open_in(&own_profile, true, [own_profile.join("source"), receiving]);
            fixture.succeeds(0, "select-all");
            fixture.succeeds(
                0,
                if cut {
                    "cut-selection"
                } else {
                    "copy-selection"
                },
            );
            fixture.succeeds(1, "paste");
            let source = fixture.folders[0].join("report");
            let destination = fixture.folders[1].join("report");
            std::fs::write(&destination, b"edited after cross-device Paste").unwrap();
            fixture.succeeds(1, "undo");
            assert!(!destination.exists());
            if cut {
                assert_eq!(
                    std::fs::read(&source).unwrap(),
                    b"edited after cross-device Paste"
                );
            }
            fixture.succeeds(1, "redo");
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"edited after cross-device Paste"
            );
            assert_eq!(source.exists(), !cut);
            fixture.succeeds(1, "undo");
            assert!(!destination.exists());
            drop(fixture);
        }
    }

    #[test]
    fn mounted_files_cut_undo_reopens_durable_receipt_after_session_restart() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cut_undo_reopens_durable_receipt_after_session_restart";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "cut-selection");
        fixture.succeeds(1, "paste");
        assert!(!fixture.folders[0].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        fixture
            .apps
            .owners
            .open(APP, &fixture.session.room)
            .unwrap()
            .try_flush()
            .unwrap();
        let tree = fixture.session.workspace_tree();
        let probe = RetiredOwner {
            tree: tree.downgrade(),
            runtime: tree.read().native_runtime.authority_witness(),
            broker: std::sync::Arc::downgrade(fixture.broker.state()),
        };
        RETIRED.with(|retired| *retired.borrow_mut() = Some(probe));
        drop(tree);
        drop(fixture);

        // Reopen the real session, broker and app records. No source bytes,
        // in-memory offer, native operation or admission token survives.
        let mut fixture = Fixture::resume(&profile);
        assert!(!fixture.folders[0].join("report").exists());
        let history = fixture.succeeds(1, "history");
        assert!(
            history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer"),
            "persisted Paste history must reopen: {history}"
        );
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            for grant in &fixture.source_grants {
                assert!(tree.workspaces[0].revoke_capability(grant));
            }
            drop(tree);
        }
        let refused = fixture.invoke(1, "undo");
        assert!(
            matches!(refused, tilo_actions::InvocationUpdate::Failed { .. }),
            "saved receipt paths cannot replace current source grants: {refused:?}"
        );
        assert!(!fixture.folders[0].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        {
            let tree = fixture.session.workspace_tree();
            tree.write().workspaces[0].grant_app(
                APP,
                &Capability::Filesystem {
                    path: fixture.folders[0].clone(),
                    mode: FsMode::ReadWrite,
                    scope: GRANT_SESSION,
                },
            );
        }
        fixture.succeeds(1, "undo");
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        assert!(!fixture.folders[1].join("report").exists());
        let history = fixture.succeeds(1, "history");
        drop(fixture);
        let paste = history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
            .unwrap();
        assert_eq!(
            paste["undone"], true,
            "only native recovery settles the persisted Undo: {history}"
        );
    }

    #[test]
    fn mounted_files_undo_compensates_exact_receipt_under_its_current_document_job() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_undo_compensates_exact_receipt_under_its_current_document_job";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        fixture.succeeds(1, "paste");
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        fixture.succeeds(1, "undo");
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        let trash = file_explorer::fileops::trash_dir().expect("the isolated home has Trash");
        let entries: Vec<_> = std::fs::read_dir(&trash)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "Copy Undo must publish one recoverable item"
        );
        let recovery = &entries[0];
        assert_eq!(std::fs::read(recovery).unwrap(), b"original source");
        let info = trash.parent().unwrap().join("info").join(format!(
            "{}.trashinfo",
            recovery.file_name().unwrap().to_str().unwrap()
        ));
        let metadata = std::fs::read_to_string(info).unwrap();
        assert!(metadata.contains("[Trash Info]\n"));
        grant_native_trash(&fixture);
        // Open the existing Trash resource and invoke its declared Restore,
        // rather than restoring through a test-only filesystem helper.
        let mut arguments = serde_json::Map::new();
        arguments.insert("path".into(), trash.to_string_lossy().into_owned().into());
        let update = fixture.invoke_with(1, "go-to", arguments);
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
            panic!("the actual Trash folder must open: {update:?}");
        };
        let listing = output.value.expect("GoTo returns the folder listing");
        assert!(
            listing["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == recovery.file_name().unwrap().to_str()),
            "the actual Trash view must list the retained object: {listing:?}"
        );
        fixture.succeeds(1, "select-all");
        let update = fixture.invoke_at(
            1,
            "restore",
            serde_json::Map::new(),
            Some(recovery.file_name().unwrap().to_str().unwrap()),
        );
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "the actual Trash row Restore must complete: {update:?}"
        );
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        assert!(!recovery.exists(), "Restore consumes the exact Trash item");
        drop(fixture);
    }

    fn grant_native_trash(fixture: &Fixture) -> Vec<String> {
        fixture.session.workspace_tree().write().workspaces[0].grant_app(
            APP,
            &Capability::Filesystem {
                path: tilo_paths::data_dir().expect("isolated user data root"),
                mode: FsMode::ReadWrite,
                scope: GRANT_SESSION,
            },
        )
    }

    fn native_trash_pair(trash: &Path) -> PathBuf {
        let mut name = trash.file_name().unwrap().to_os_string();
        name.push(".trashinfo");
        trash
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("info")
            .join(name)
    }

    fn edited_copy_in_native_trash(fixture: &mut Fixture) -> PathBuf {
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        fixture.succeeds(1, "paste");
        std::fs::write(
            fixture.folders[1].join("report"),
            b"edited after Paste before Undo",
        )
        .unwrap();
        fixture.succeeds(1, "undo");
        let root = tilo_paths::data_dir().unwrap().join("Trash/files");
        let entries = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "one actual host Trash item");
        assert_eq!(
            std::fs::read(&entries[0]).unwrap(),
            b"edited after Paste before Undo"
        );
        entries[0].clone()
    }

    fn restore_native_entry(fixture: &mut Fixture, trash: &Path) -> tilo_actions::InvocationUpdate {
        fixture.invoke_at(
            1,
            "restore",
            serde_json::Map::new(),
            Some(&trash.file_name().unwrap().to_string_lossy()),
        )
    }

    #[test]
    fn mounted_files_native_trash_restore_preserves_edited_bytes_and_cold_history() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_trash_restore_preserves_edited_bytes_and_cold_history";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        let trash = edited_copy_in_native_trash(&mut fixture);
        let info = native_trash_pair(&trash);
        grant_native_trash(&fixture);
        let before = fixture.succeeds(1, "clipboard-status");
        let listing = fixture.succeeds(1, "open-trash");
        assert!(
            listing["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == trash.file_name().unwrap().to_str())
        );
        let update = restore_native_entry(&mut fixture, &trash);
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "{update:?}"
        );
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"edited after Paste before Undo"
        );
        assert!(!trash.exists());
        assert!(!info.exists());
        assert_eq!(
            fixture.succeeds(1, "clipboard-status"),
            before,
            "Restore keeps the workspace clipboard offer"
        );
        let history = fixture.succeeds(1, "history");
        assert!(
            history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["what"]["action"]["action"] == "record-trash-restore"),
            "{history}"
        );
        fixture
            .apps
            .owners
            .open(APP, &fixture.session.room)
            .unwrap()
            .try_flush()
            .unwrap();
        drop(fixture);
        let mut fixture = Fixture::resume(&profile);
        let refused = fixture.invoke(1, "undo");
        assert!(
            matches!(refused, tilo_actions::InvocationUpdate::Failed { .. }),
            "saved Restore cannot create a new Trash grant: {refused:?}"
        );
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"edited after Paste before Undo"
        );
        grant_native_trash(&fixture);
        fixture.succeeds(1, "undo");
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(&trash).unwrap(),
            b"edited after Paste before Undo"
        );
        assert!(info.is_file());
        fixture
            .apps
            .owners
            .open(APP, &fixture.session.room)
            .unwrap()
            .try_flush()
            .unwrap();
        drop(fixture);
        let mut fixture = Fixture::resume(&profile);
        grant_native_trash(&fixture);
        fixture.succeeds(1, "redo");
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"edited after Paste before Undo"
        );
        assert!(!trash.exists());
        assert!(!info.exists());
        drop(fixture);
    }

    #[test]
    fn mounted_files_native_trash_restore_recovers_after_process_kill() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_trash_restore_recovers_after_process_kill";
        native_trash_restore_kill_journey(TEST, "cut-before-remove");
    }

    #[test]
    fn mounted_files_native_trash_restore_recovers_held_metadata_after_process_kill() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_trash_restore_recovers_held_metadata_after_process_kill";
        native_trash_restore_kill_journey(TEST, "restore-info-retained");
    }

    fn native_trash_restore_kill_journey(test: &str, phase: &str) {
        let Some(profile) = isolated_session_root(test) else {
            return;
        };
        if let Some(receiving) = std::env::var_os("TILO_CLIPBOARD_TEST_CROSS_DEVICE") {
            let mut fixture = Fixture::open_in(
                &profile,
                true,
                [profile.join("source"), PathBuf::from(receiving)],
            );
            let trash = edited_copy_in_native_trash(&mut fixture);
            grant_native_trash(&fixture);
            fixture.succeeds(1, "open-trash");
            fixture
                .apps
                .owners
                .open(APP, &fixture.session.room)
                .unwrap()
                .try_flush()
                .unwrap();
            std::fs::write(
                profile.join("restore-trash-path"),
                trash.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            restore_native_entry(&mut fixture, &trash);
            drop(fixture);
            panic!("the actual cross-device Restore phase was not reached");
        }
        std::fs::create_dir_all(&profile).unwrap();
        let remote_mount = tempfile::Builder::new()
            .prefix("tilo-restore-cross-device-")
            .tempdir_in("/dev/shm")
            .expect("a real second filesystem is required");
        assert_ne!(
            std::fs::metadata(&profile).unwrap().dev(),
            std::fs::metadata(remote_mount.path()).unwrap().dev()
        );
        recover_killed_restore(
            &profile.join("interrupted"),
            &remote_mount.path().join("receiving"),
            phase,
            test,
        );
    }

    fn recover_killed_restore(root: &Path, receiving: &Path, phase: &str, test: &str) {
        use std::os::unix::ffi::OsStringExt as _;
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("TILO_IN_PROCESS_TEST_ROOT", root)
            .env("TILO_CLIPBOARD_TEST_CROSS_DEVICE", receiving)
            .env("TILO_CLIPBOARD_TEST_KILL_AT", phase)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let trash = PathBuf::from(std::ffi::OsString::from_vec(
            std::fs::read(root.join("restore-trash-path")).unwrap(),
        ));
        let info = native_trash_pair(&trash);
        let mut held = info.as_os_str().to_os_string();
        held.push(".tilo-restore-held");
        let held = PathBuf::from(held);
        if phase == "restore-info-retained" {
            assert!(!info.exists());
            assert!(held.is_file());
            assert_eq!(
                std::fs::read(receiving.join("report")).unwrap(),
                b"edited after Paste before Undo"
            );
        }
        let mut fixture =
            Fixture::open_in(root, false, [root.join("source"), receiving.to_path_buf()]);
        grant_native_trash(&fixture);
        assert_eq!(
            fixture.succeeds(1, "recover-clipboard-transfers")["completed"],
            1
        );
        assert!(!receiving.join("report").exists());
        assert_eq!(
            std::fs::read(&trash).unwrap(),
            b"edited after Paste before Undo"
        );
        assert!(
            info.is_file(),
            "recovery must retain matching standard Trash metadata"
        );
        assert!(
            !held.exists(),
            "recovery must return its exact held metadata"
        );
        fixture.succeeds(1, "open-trash");
        assert!(matches!(
            restore_native_entry(&mut fixture, &trash),
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        assert_eq!(
            std::fs::read(receiving.join("report")).unwrap(),
            b"edited after Paste before Undo"
        );
        assert!(!trash.exists());
        assert!(!info.exists());
        drop(fixture);
    }

    #[test]
    fn mounted_files_native_trash_restore_refuses_conflicts_and_revoked_grants() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_trash_restore_refuses_conflicts_and_revoked_grants";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        let trash = edited_copy_in_native_trash(&mut fixture);
        let info = native_trash_pair(&trash);
        let metadata = std::fs::read(&info).unwrap();
        let grants = grant_native_trash(&fixture);
        let before = fixture.succeeds(1, "clipboard-status");
        fixture.succeeds(1, "open-trash");
        let destination = fixture.folders[1].join("report");
        std::fs::write(&destination, b"new occupant").unwrap();
        assert!(matches!(
            restore_native_entry(&mut fixture, &trash),
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"new occupant");
        assert_eq!(
            std::fs::read(&trash).unwrap(),
            b"edited after Paste before Undo"
        );
        assert_eq!(std::fs::read(&info).unwrap(), metadata);
        std::fs::remove_file(&destination).unwrap();
        for grant in grants {
            assert!(
                fixture.session.workspace_tree().write().workspaces[0].revoke_capability(&grant)
            );
        }
        assert!(matches!(
            restore_native_entry(&mut fixture, &trash),
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read(&trash).unwrap(),
            b"edited after Paste before Undo"
        );
        assert_eq!(std::fs::read(&info).unwrap(), metadata);
        grant_native_trash(&fixture);
        assert!(matches!(
            restore_native_entry(&mut fixture, &trash),
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"edited after Paste before Undo"
        );
        assert_eq!(fixture.succeeds(1, "clipboard-status"), before);
        drop(fixture);
    }

    fn paused_restore_refuses(fixture: &mut Fixture, trash: &Path, case: &str) {
        use crate::shell::broker_services::TestTransferPause;
        let info = native_trash_pair(trash);
        let original = std::fs::read(&info).unwrap();
        let grants = grant_native_trash(fixture);
        let destination = fixture.folders[1].join("report");
        let pause = TestTransferPause::new(&fixture.folders[1], "restore-before-transfer");
        let (job, invocation) = fixture.start_invoke_at(
            1,
            "restore",
            serde_json::Map::new(),
            Some(&trash.file_name().unwrap().to_string_lossy()),
        );
        {
            let [first, second] = &mut fixture.shown;
            until(
                &mut [first, second],
                "admitted Restore reaches the real transfer boundary",
                || pause.entered(),
            );
        }
        match case {
            "revoked" => {
                for grant in grants {
                    assert!(
                        fixture.session.workspace_tree().write().workspaces[0]
                            .revoke_capability(&grant)
                    );
                }
            }
            "metadata" => {
                let mut changed = original.clone();
                changed.extend_from_slice(b"\n# changed after admission\n");
                std::fs::write(&info, changed).unwrap();
            }
            "occupied" => {
                std::fs::write(&destination, b"new occupant").unwrap();
            }
            "cancel" => {
                fixture.host.cancel(&invocation).unwrap();
            }
            _ => panic!("unknown Restore journey {case}"),
        }
        let expected_metadata = std::fs::read(&info).unwrap();
        pause.release();
        {
            let [first, second] = &mut fixture.shown;
            until(&mut [first, second], "actual Restore worker stops", || {
                pause.finished()
            });
        }
        let update = fixture.settle_job(job);
        if case == "cancel" {
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }),
                "{update:?}"
            );
        } else {
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Failed { .. }),
                "{case}: {update:?}"
            );
        }
        assert_eq!(
            std::fs::read(trash).unwrap(),
            b"edited after Paste before Undo",
            "{case}"
        );
        assert_eq!(std::fs::read(&info).unwrap(), expected_metadata, "{case}");
        if case == "occupied" {
            assert_eq!(std::fs::read(&destination).unwrap(), b"new occupant");
            std::fs::remove_file(destination).unwrap();
        } else {
            assert!(!destination.exists(), "{case} must publish no destination");
        }
        if case == "metadata" {
            std::fs::write(info, original).unwrap();
        }
    }

    #[test]
    fn mounted_files_native_trash_restore_rechecks_admitted_objects_and_cancellation() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_trash_restore_rechecks_admitted_objects_and_cancellation";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        let trash = edited_copy_in_native_trash(&mut fixture);
        grant_native_trash(&fixture);
        let before = fixture.succeeds(1, "clipboard-status");
        fixture.succeeds(1, "open-trash");
        let page = fixture.host.query(tilo_actions::remote::CatalogueQuery {
            provider: Some(fixture.providers[1].clone()),
            needle: "delete-permanently".into(),
            ..Default::default()
        });
        let page = fixture.wait(page).unwrap();
        assert!(
            !page.rows.iter().any(|row| {
                row.resource.kind == "files.entry"
                    && row.resource.label == trash.file_name().unwrap().to_string_lossy()
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.address.action == "delete-permanently")
            }),
            "host Trash must not offer a private-namespace purge"
        );
        for case in ["revoked", "metadata", "occupied", "cancel"] {
            paused_restore_refuses(&mut fixture, &trash, case);
        }
        grant_native_trash(&fixture);
        assert!(matches!(
            restore_native_entry(&mut fixture, &trash),
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        assert_eq!(fixture.succeeds(1, "clipboard-status"), before);
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"edited after Paste before Undo"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_paste_refuses_a_replaced_receiving_folder() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_paste_refuses_a_replaced_receiving_folder";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        let original = profile.join("original-receiving");
        std::fs::rename(&fixture.folders[1], &original).unwrap();
        std::fs::create_dir(&fixture.folders[1]).unwrap();
        assert!(matches!(
            fixture.invoke(1, "paste"),
            tilo_actions::InvocationUpdate::Failed { .. }
        ));
        assert_eq!(std::fs::read_dir(&fixture.folders[1]).unwrap().count(), 0);
        assert_eq!(
            std::fs::read(original.join("other")).unwrap(),
            b"receiving file"
        );
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_cut_paste_consumes_offer_and_undo_restores_the_source() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cut_paste_consumes_offer_and_undo_restores_the_source";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "cut-selection");
        assert!(fixture.folders[0].join("report").exists());
        fixture.succeeds(1, "paste");
        assert!(!fixture.folders[0].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        assert!(fixture.succeeds(1, "clipboard-status")["offer"].is_null());
        fixture.succeeds(1, "undo");
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_range_undo_compensates_the_paste_after_a_checkpoint() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_range_undo_compensates_the_paste_after_a_checkpoint";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        let checkpoint = fixture.invoke_with(
            1,
            "checkpoint",
            serde_json::json!({"name": "before-paste"})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(matches!(
            checkpoint,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        fixture.succeeds(1, "paste");
        let undo = fixture.invoke_with(
            1,
            "undo",
            serde_json::json!({"to": "before-paste"})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(
            matches!(undo, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "{undo:?}"
        );
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_person_undo_compensates_an_actual_agent_paste() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_person_undo_compensates_an_actual_agent_paste";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            tree.workspaces[0].grant_capability(crate::shell::state::CapOverride::membership(
                crate::shell::state::Subject::agent("clipboard-helper"),
                fixture.session.room.clone(),
                crate::shell::state::Verb::Write,
            ));
            tree.workspaces[0].grant_capability(crate::shell::state::CapOverride::grant(
                crate::shell::state::Subject::agent("clipboard-helper"),
                crate::shell::state::Scope::App(APP.into()),
                crate::shell::state::Verb::Write,
            ));
            drop(tree);
        }
        let agent = AgentDoor::open(&fixture.session, "clipboard-helper");
        for (index, action) in [(0, "copy"), (1, "paste")] {
            let update = fixture.agent_invoke(&agent, index, action);
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{action}: {update:?}"
            );
        }
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        drop(agent);
        let history = fixture.succeeds(1, "history");
        let receipt = history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
            .expect("the actual agent paste is recorded");
        assert_eq!(receipt["by"], "agent:clipboard-helper", "{receipt}");
        let author = receipt["by"].as_str().unwrap();
        let update = fixture.invoke_with(
            1,
            "undo",
            serde_json::json!({"by": author})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "human Undo of the actual agent step failed: {update:?}"
        );
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        drop(fixture);
    }

    #[test]
    fn mounted_files_cut_undo_refuses_revoked_source_access_and_preserves_bytes() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_cut_undo_refuses_revoked_source_access_and_preserves_bytes";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "cut-selection");
        fixture.succeeds(1, "paste");
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            for grant in &fixture.source_grants {
                assert!(tree.workspaces[0].revoke_capability(grant));
            }
            drop(tree);
        }
        let update = fixture.invoke(1, "undo");
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Failed { .. }),
            "Undo may not claim source restoration after its grant was revoked: {update:?}"
        );
        assert!(!fixture.folders[0].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        let history = fixture.succeeds(1, "history");
        let paste = history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
            .expect("the failed compensation retains its original Paste history");
        assert_ne!(
            paste["undone"], true,
            "a refused native Undo must not claim its filesystem transfer was undone: {history}"
        );
        {
            let tree = fixture.session.workspace_tree();
            let mut tree = tree.write();
            tree.workspaces[0].grant_app(
                APP,
                &Capability::Filesystem {
                    path: fixture.folders[0].clone(),
                    mode: FsMode::ReadWrite,
                    scope: GRANT_SESSION,
                },
            );
        }
        fixture.succeeds(1, "undo");
        assert!(!fixture.folders[1].join("report").exists());
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        let history = fixture.succeeds(1, "history");
        let paste = history["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["what"]["action"]["action"] == "record-clipboard-transfer")
            .unwrap();
        assert_eq!(
            paste["undone"], true,
            "only the completed retry marks history undone: {history}"
        );
        drop(fixture);
    }

    fn restore_and_capture_native_copy(
        fixture: &mut Fixture,
        renderer: &mut ui::offscreen::OffscreenRenderer,
        themes: &[(&str, ui::theme::Theme)],
        edited: &[u8],
    ) {
        grant_native_trash(fixture);
        fixture.succeeds(1, "open-trash");
        let retained = std::fs::read_dir(tilo_paths::data_dir().unwrap().join("Trash/files"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "one exact retained native Copy");
        let retained = &retained[0];
        assert_eq!(std::fs::read(retained).unwrap(), edited);
        for (name, theme) in themes {
            fixture.capture(renderer, theme, &format!("before-trash-restore-{name}"));
        }
        let update = restore_native_entry(fixture, retained);
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "actual native entry Restore must complete: {update:?}"
        );
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            edited
        );
        assert!(!retained.exists());
        assert!(!native_trash_pair(retained).exists());
        let receiving = fixture.folders[1].clone();
        let update = fixture.invoke_with(
            1,
            "go-to",
            serde_json::json!({"path": receiving.to_string_lossy()})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        for (name, theme) in themes {
            fixture.capture(renderer, theme, &format!("after-trash-restore-{name}"));
        }
        let history = fixture.succeeds(1, "history");
        assert!(
            history["versions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["what"]["action"]["action"] == "record-trash-restore"),
            "actual Restore must be in shared history: {history}"
        );
    }

    #[test]
    fn mounted_files_native_paste_renders_its_receipt_in_three_themes() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_native_paste_renders_its_receipt_in_three_themes";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        let mut renderer = ui::offscreen::OffscreenRenderer::try_new(1800, 600)
            .expect("offscreen evidence must observe a usable remote render adapter");
        fixture.succeeds(0, "select-all");
        fixture.succeeds(0, "copy-selection");
        let themes = [
            ("light", ui::theme::Theme::light()),
            ("dark", ui::theme::Theme::dark()),
            ("high-contrast", ui::theme::Theme::high_contrast()),
        ];
        for (name, theme) in &themes {
            fixture.capture(&mut renderer, theme, &format!("before-paste-{name}"));
        }
        fixture.succeeds(1, "paste");
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        for (name, theme) in &themes {
            fixture.capture(&mut renderer, theme, &format!("after-paste-{name}"));
        }
        fixture.succeeds(1, "undo");
        assert!(!fixture.folders[1].join("report").exists());
        for (name, theme) in &themes {
            fixture.capture(&mut renderer, theme, &format!("after-undo-{name}"));
        }
        fixture.succeeds(1, "redo");
        assert_eq!(
            std::fs::read(fixture.folders[1].join("report")).unwrap(),
            b"original source"
        );
        for (name, theme) in &themes {
            fixture.capture(&mut renderer, theme, &format!("after-redo-{name}"));
        }
        let edited = b"edited after Paste before Undo";
        std::fs::write(fixture.folders[1].join("report"), edited).unwrap();
        fixture.succeeds(1, "undo");
        let trash = file_explorer::fileops::trash_dir().expect("the isolated home has Trash");
        let update = fixture.invoke_with(
            1,
            "go-to",
            serde_json::json!({"path": trash.to_string_lossy()})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "Trash is reached through its declared action: {update:?}"
        );
        for (name, theme) in &themes {
            fixture.capture(&mut renderer, theme, &format!("trash-recovery-{name}"));
        }
        restore_and_capture_native_copy(&mut fixture, &mut renderer, &themes, edited);
        drop(fixture);
    }

    #[test]
    fn mounted_files_keyboard_copy_paste_and_undo_reach_the_native_host() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_keyboard_copy_paste_and_undo_reach_the_native_host";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        fixture.command_key(0, "c");
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .expect("bounded test deadline");
        while fixture.succeeds(1, "clipboard-status")["offer"].is_null() {
            assert!(
                std::time::Instant::now() < deadline,
                "keyboard Copy never published an actual native offer"
            );
        }
        fixture.command_key(1, "v");
        let pasted = fixture.folders[1].join("report");
        let [first, second] = &mut fixture.shown;
        until(
            &mut [first, second],
            "keyboard Paste writes its actual destination",
            || pasted.exists(),
        );
        assert_eq!(std::fs::read(&pasted).unwrap(), b"original source");
        fixture.wait_for_paste_history(
            None,
            "keyboard Paste must record its actual receipt before Undo",
        );
        fixture.command_key(1, "z");
        let [first, second] = &mut fixture.shown;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .expect("bounded test deadline");
        until(
            &mut [first, second],
            "keyboard Undo compensates the pasted object",
            || {
                assert!(
                    std::time::Instant::now() < deadline,
                    "keyboard Undo left its receipted native copy in place"
                );
                !pasted.exists()
            },
        );
        assert_eq!(
            std::fs::read(fixture.folders[0].join("report")).unwrap(),
            b"original source"
        );
        fixture.wait_for_paste_history(
            Some(true),
            "keyboard Undo must settle its actual native receipt before Redo",
        );
        // Ctrl+Shift+Z must use the same freshly admitted native Redo as
        // the semantic action, with no document-patch fallback.
        fixture.command_key_with_modifiers(
            1,
            "z",
            ui::winit::keyboard::ModifiersState::CONTROL
                | ui::winit::keyboard::ModifiersState::SHIFT,
        );
        let [first, second] = &mut fixture.shown;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .expect("bounded restart deadline");
        until(
            &mut [first, second],
            "keyboard Redo restores the exact copied object",
            || {
                assert!(
                    std::time::Instant::now() < deadline,
                    "keyboard Redo did not restore its native receipt"
                );
                pasted.exists()
            },
        );
        assert_eq!(std::fs::read(&pasted).unwrap(), b"original source");
        drop(fixture);
    }

    #[test]
    fn mounted_files_copy_refuses_a_name_reused_after_semantic_selection() {
        const TEST: &str = "shell::in_process::tests::files_clipboard_tests::mounted_files_copy_refuses_a_name_reused_after_semantic_selection";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        let mut fixture = Fixture::new(&profile);
        fixture.succeeds(0, "select-all");
        let selected = fixture.folders[0].join("report");
        let original = profile.join("original-report");
        std::fs::rename(&selected, &original).unwrap();
        std::fs::write(&selected, b"replacement must not be copied").unwrap();
        let update = fixture.invoke(0, "copy-selection");
        let tilo_actions::InvocationUpdate::Failed { error, .. } = update else {
            panic!("the actual host must refuse the replaced shown source: {update:?}");
        };
        assert!(
            error
                .message
                .contains("shown object differs from its retained selection"),
            "{error:?}"
        );
        assert!(fixture.succeeds(1, "clipboard-status")["offer"].is_null());
        assert_eq!(std::fs::read(original).unwrap(), b"original source");
        assert_eq!(
            std::fs::read(selected).unwrap(),
            b"replacement must not be copied"
        );
        assert_eq!(std::fs::read_dir(&fixture.folders[1]).unwrap().count(), 1);
        drop(fixture);
    }
}
