//! Real local sockets, the native launcher record and production host service.
//! This does not mount a Files surface or qualify SDK history integration.
use super::*;
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
use ed25519_dalek::{Signer as _, SigningKey};
use runtime_collab::file_clipboard::ObjectIdentity;
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use std::os::unix::fs::PermissionsExt as _;

const APP: &str = "so.tilo.file-explorer";

struct Fixture {
    directory: tempfile::TempDir,
    socket: PathBuf,
    room: String,
    identity: Vec<u8>,
    tree: WorkspaceTreeHandle,
    clipboard: Arc<NativeFileClipboard>,
    _broker: BrokerHandle,
    /// This test's own data home, where its Trash lives: never the host's,
    /// nor one another test running alongside creates at the same time.
    _data_home: tilo_test_env::ScopedEnv,
    _data: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().expect("private data home");
        fs::set_permissions(data.path(), fs::Permissions::from_mode(0o700))
            .expect("owner-only data home");
        let mut data_home = tilo_test_env::ScopedEnv::new();
        data_home.set("XDG_DATA_HOME", data.path());
        let directory = tempfile::tempdir().expect("private clipboard fixture");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("owner-only fixture socket directory");
        let socket = directory.path().join("broker.sock");
        let signing_root = SigningKey::from_bytes(&[119; 32]);
        let device = SigningKey::from_bytes(&[120; 32]);
        let mut workspace = Workspace::new(WorkspaceId(119), "Clipboard bridge fixture");
        workspace.set_owner(
            tilo_action_broker::collab_broker_capnp::principal_id_for_root(
                signing_root.verifying_key().to_bytes(),
            ),
        );
        workspace.grant_app(
            APP,
            &Capability::Filesystem {
                path: directory.path().to_path_buf(),
                mode: FsMode::ReadWrite,
                scope: crate::shell::state::GRANT_SESSION,
            },
        );
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree {
            persist_path: Some(directory.path().join("state.json")),
            ..Default::default()
        };
        tree.workspaces.push(workspace);
        let tree = tree.shared();
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: signing_root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Clipboard fixture device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &signing_root)
            .expect("signed certificate");
        let identity = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
            proof: device
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("certified identity");
        let clipboard = Arc::new(NativeFileClipboard::new(tree.clone()));
        let mut services = super::super::native_services(Some(tree.clone()), None, None);
        services.file_clipboard = Some(clipboard.clone());
        // This fixture enrolls its signed device explicitly through the broker's
        // test-support configuration; it has no installed native identity host.
        // Launcher admission, live workspace policy and the clipboard service
        // all remain the production implementations.
        services.native_certificates = None;
        let broker = tilo_action_broker::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(Arc::new(super::super::NativeWorkspaceAuthority(
                tree.clone(),
            ))),
            tilo_action_broker::collab_outbound::PeerPublisher::disabled(),
            None,
            services,
            Some(Arc::new(vec![device.verifying_key().to_bytes()])),
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("production broker with enrolled fixture device");
        tilo_test_env::wait_until("clipboard broker socket", || socket.exists());
        Self {
            directory,
            socket,
            room,
            identity,
            tree,
            clipboard,
            _broker: broker,
            _data_home: data_home,
            _data: data,
        }
    }
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn connect(&self, record: &str) -> Connection {
        crate::shell::launcher::register_in_process_launch(APP, &self.room);
        connect(&self.socket, &self.room, &self.identity, record).await
    }
}
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on this test's LocalSet"
)]
async fn connect(socket: &Path, room: &str, identity: &[u8], record: &str) -> Connection {
    use tokio::io::AsyncWriteExt as _;
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("local socket");
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": APP, "workspace_id": room,
    }))
    .expect("launcher handoff");
    stream
        .write_all(&launcher::services::COLLAB_LAUNCH_MAGIC)
        .await
        .expect("marker");
    stream
        .write_all(
            &launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .expect("framed handoff"),
        )
        .await
        .expect("handoff");
    // This connection reaches the broker itself, so it carries its own lane.
    let lane = runtime_portal::descriptor_lane::DescriptorSender::new();
    let stream = runtime_portal::descriptor_lane::DescriptorStream::sending(stream, lane.clone());
    connect_stream(stream, lane, room, identity, record).await
}
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on the test LocalSet"
)]
async fn connect_stream<S>(
    stream: S,
    lane: runtime_portal::descriptor_lane::DescriptorSender,
    room: &str,
    identity: &[u8],
    record: &str,
) -> Connection
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let network = Box::new(capnp_rpc::twoparty::VatNetwork::new(
        futures_util::io::BufReader::new(reader.compat()),
        futures_util::io::BufWriter::new(writer.compat_write()),
        capnp_rpc::rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc = capnp_rpc::RpcSystem::new(network, None);
    let client = runtime_collab::CollabClient::from_raw(
        rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server),
    )
    .with_descriptors(lane);
    let driver = tokio::task::spawn_local(async move {
        let _ = rpc.await;
    });
    let route = tilo_action_broker::route::CrdtRoute::record(APP, room, record);
    let channel = client
        .join_channel(&route.channel_key(), identity)
        .await
        .expect("attested Files join");
    Connection {
        channel,
        client,
        driver,
    }
}

mod child_tests;

struct Connection {
    channel: runtime_collab::ChannelHandle,
    client: runtime_collab::CollabClient,
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl Connection {
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn command(&self, command: Command) -> Result<Reply, String> {
        self.channel
            .local_file_clipboard_command(&command, None)
            .await
            .map_err(|error| error.to_string())
    }
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn capture(&self, path: &Path, destination: bool) -> String {
        let (_retained, descriptor) = selected(path);
        let command = if destination {
            Command::Capture {
                sources: vec![],
                destination: Some(descriptor),
            }
        } else {
            Command::Capture {
                sources: vec![descriptor],
                destination: None,
            }
        };
        match self.command(command).await.expect("actual ticket capture") {
            Reply::Captured {
                sources,
                destination,
            } => {
                if let Some(destination) = destination {
                    destination
                } else {
                    sources.into_iter().next().expect("source ticket")
                }
            }
            other => panic!("capture returned {other:?}"),
        }
    }
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn publish(&self, source: String, cut: bool) -> String {
        let Reply::Published { offer } = self
            .command(Command::Publish {
                sources: vec![source],
                cut,
            })
            .await
            .expect("publish exact ticket")
        else {
            panic!("published offer");
        };
        offer.id
    }
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn compensate(&self, command: Command) -> Result<Reply, String> {
        let operation = match &command {
            Command::Undo { operation } | Command::Redo { operation } => operation.clone(),
            _ => panic!("expected compensation"),
        };
        let reply = self.command(command).await?;
        assert_eq!(
            reply,
            Reply::Running {
                operation: operation.clone()
            }
        );
        Ok(self.settled(&operation).await)
    }

    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn settled(&self, operation: &str) -> Reply {
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            loop {
                let reply = self
                    .command(Command::Observe {
                        operation: operation.into(),
                    })
                    .await
                    .expect("receipt");
                if matches!(reply, Reply::Settled { .. }) {
                    break reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded clipboard worker settlement")
    }
}
fn selected(path: &Path) -> (File, SelectedDescriptor) {
    let entry = SecureNearbySource::open_beneath(path, path)
        .expect("retain shown object")
        .into_file();
    let metadata = entry.metadata().expect("shown identity");
    let descriptor = SelectedDescriptor {
        path: path.as_os_str().as_bytes().to_vec(),
        descriptor: u32::try_from(entry.as_raw_fd()).expect("selected descriptor"),
        expected: ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    };
    (entry, descriptor)
}

#[test]
fn clipboard_bridge_refuses_replacement_before_capture_and_foreign_window_tickets() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let b = fixture.connect("window-b").await;
        let path = fixture.directory.path().join("selected");
        fs::write(&path, b"original").expect("original source");
        let (_retained, descriptor) = selected(&path);
        fs::rename(&path, fixture.directory.path().join("original")).expect("replace path");
        fs::write(&path, b"replacement").expect("replacement source");
        let error = a
            .command(Command::Capture {
                sources: vec![descriptor],
                destination: None,
            })
            .await
            .expect_err("replacement before capture must refuse");
        assert!(error.contains("retained selection"), "{error}");
        let (_retained, descriptor) = selected(&path);
        let Reply::Captured { sources, .. } = a
            .command(Command::Capture {
                sources: vec![descriptor],
                destination: None,
            })
            .await
            .expect("capture exact shown source")
        else {
            panic!("source tickets");
        };
        for command in [
            Command::Publish {
                sources: sources.clone(),
                cut: false,
            },
            Command::Release {
                tickets: sources.clone(),
            },
        ] {
            let error = b
                .command(command)
                .await
                .expect_err("foreign window must refuse");
            assert!(error.contains("another Files window"), "{error}");
        }
        assert!(
            matches!(
                a.command(Command::Publish {
                    sources,
                    cut: false
                })
                .await,
                Ok(Reply::Published { .. })
            ),
            "foreign release did not discard original ticket"
        );
    });
}

#[test]
fn clipboard_bridge_pastes_exact_tickets_and_observes_retry_without_duplicate_copy() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let b = fixture.connect("window-b").await;
        let path = fixture.directory.path().join("report");
        let folder = fixture.directory.path().join("receiving");
        fs::write(&path, b"actual copied bytes").expect("source");
        fs::create_dir(&folder).expect("receiving folder");
        let (_source, descriptor) = selected(&path);
        let Reply::Captured { sources, .. } = a
            .command(Command::Capture {
                sources: vec![descriptor],
                destination: None,
            })
            .await
            .expect("capture source")
        else {
            panic!("source ticket");
        };
        let Reply::Published { offer } = a
            .command(Command::Publish {
                sources,
                cut: false,
            })
            .await
            .expect("publish Copy")
        else {
            panic!("offer");
        };
        let (_folder, descriptor) = selected(&folder);
        let Reply::Captured {
            destination: Some(destination),
            ..
        } = b
            .command(Command::Capture {
                sources: vec![],
                destination: Some(descriptor),
            })
            .await
            .expect("capture destination")
        else {
            panic!("destination ticket");
        };
        let operation = uuid::Uuid::new_v4().to_string();
        let paste = Command::Paste {
            offer: offer.id,
            destination,
            operation: operation.clone(),
            conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
        };
        assert!(matches!(
            b.command(paste.clone()).await,
            Ok(Reply::Running { .. })
        ));
        let receipt = b.settled(&operation).await;
        assert!(matches!(
            receipt,
            Reply::Settled {
                completed: 1,
                error: None,
                undone: false,
                ..
            }
        ));
        assert_eq!(
            fs::read(folder.join("report")).expect("published bytes"),
            b"actual copied bytes"
        );
        assert_eq!(
            b.command(paste).await.expect("same-operation retry"),
            receipt
        );
        assert_eq!(
            fs::read_dir(&folder).expect("receiving contents").count(),
            1
        );
        fixture.tree.write().workspaces[0].sandbox.clear();
        let reply = b
            .compensate(Command::Undo { operation })
            .await
            .expect("compensation receipt");
        assert!(
            matches!(reply, Reply::Settled {
            completed: 1, error: Some(ref error), undone: false, ..
        } if error.contains("revoked")),
            "{reply:?}"
        );
        assert_eq!(
            fs::read(folder.join("report")).expect("revoked Undo preserves destination"),
            b"actual copied bytes"
        );
    });
}

#[test]
fn clipboard_bridge_refuses_replaced_destination_before_capture_and_paste() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let b = fixture.connect("window-b").await;
        let source = fixture.directory.path().join("source");
        let folder = fixture.directory.path().join("receiving");
        fs::write(&source, b"original bytes").expect("source");
        fs::create_dir(&folder).expect("folder");
        let (_retained, descriptor) = selected(&folder);
        fs::rename(&folder, fixture.directory.path().join("original-folder"))
            .expect("replace folder");
        fs::create_dir(&folder).expect("replacement folder");
        let error = b
            .command(Command::Capture {
                sources: vec![],
                destination: Some(descriptor),
            })
            .await
            .expect_err("replacement before Capture refuses");
        assert!(error.contains("retained selection"), "{error}");
        let ticket = a.capture(&source, false).await;
        let offer = a.publish(ticket, false).await;
        let destination = b.capture(&folder, true).await;
        fs::rename(&folder, fixture.directory.path().join("captured-folder"))
            .expect("replace captured folder");
        fs::create_dir(&folder).expect("second replacement");
        let error = b
            .command(Command::Paste {
                offer,
                destination,
                operation: uuid::Uuid::new_v4().to_string(),
                conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
            })
            .await
            .expect_err("replacement after Capture refuses");
        assert!(error.contains("destination folder was replaced"), "{error}");
        for folder in [
            folder,
            fixture.directory.path().join("original-folder"),
            fixture.directory.path().join("captured-folder"),
        ] {
            assert_eq!(fs::read_dir(folder).expect("receiving contents").count(), 0);
        }
    });
}

#[test]
fn clipboard_bridge_expired_ticket_and_closed_source_cannot_publish_or_paste() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let b = fixture.connect("window-b").await;
        let source = fixture.directory.path().join("source");
        let folder = fixture.directory.path().join("receiving");
        fs::write(&source, b"original bytes").expect("source");
        fs::create_dir(&folder).expect("folder");
        let ticket = a.capture(&source, false).await;
        {
            let mut state = fixture.clipboard.state.lock().expect("host ticket clock");
            let ticket = state.tickets.get_mut(&ticket).expect("captured ticket");
            Arc::get_mut(ticket).expect("unspent ticket").expires = Instant::now();
            drop(state);
        }
        let error = a
            .command(Command::Publish {
                sources: vec![ticket],
                cut: false,
            })
            .await
            .expect_err("expired ticket refuses");
        assert!(error.contains("ticket expired"), "{error}");
        let ticket = a.capture(&source, false).await;
        let offer = a.publish(ticket, false).await;
        let destination = b.capture(&folder, true).await;
        drop(a);
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            loop {
                if matches!(
                    b.command(Command::Snapshot).await,
                    Ok(Reply::Snapshot { offer: None })
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual source connection closure invalidates offer");
        let error = b
            .command(Command::Paste {
                offer,
                destination,
                operation: uuid::Uuid::new_v4().to_string(),
                conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
            })
            .await
            .expect_err("closed source cannot paste");
        assert!(error.contains("offer replaced or unavailable"), "{error}");
        assert_eq!(fs::read_dir(folder).expect("receiving contents").count(), 0);
    });
}

#[test]
fn clipboard_bridge_cut_restores_exact_receipt_under_current_undo_authority() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let b = fixture.connect("window-b").await;
        let source = fixture.directory.path().join("source");
        let folder = fixture.directory.path().join("receiving");
        fs::write(&source, b"moved bytes").expect("source");
        fs::create_dir(&folder).expect("folder");
        let ticket = a.capture(&source, false).await;
        let offer = a.publish(ticket, true).await;
        let destination = b.capture(&folder, true).await;
        let operation = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            b.command(Command::Paste {
                offer,
                destination,
                operation: operation.clone(),
                conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
            })
            .await,
            Ok(Reply::Running { .. })
        ));
        assert!(matches!(
            b.settled(&operation).await,
            Reply::Settled {
                completed: 1,
                error: None,
                undone: false,
                ..
            }
        ));
        assert!(!source.exists());
        assert_eq!(
            fs::read(folder.join("source")).expect("Cut receipt bytes"),
            b"moved bytes"
        );
        fs::write(&source, b"new unrelated source").expect("source name reused");
        let reply = b
            .compensate(Command::Undo {
                operation: operation.clone(),
            })
            .await
            .expect("refused compensation receipt");
        assert!(
            matches!(
                reply,
                Reply::Settled {
                    completed: 1,
                    error: Some(_),
                    undone: false,
                    ..
                }
            ),
            "{reply:?}"
        );
        assert_eq!(
            fs::read(&source).expect("unrelated source preserved"),
            b"new unrelated source"
        );
        assert_eq!(
            fs::read(folder.join("source")).expect("receipt preserved"),
            b"moved bytes"
        );
        fs::remove_file(&source).expect("remove unrelated fixture");
        assert!(matches!(
            b.compensate(Command::Undo {
                operation: operation.clone()
            })
            .await,
            Ok(Reply::Settled {
                completed: 1,
                error: None,
                undone: true,
                ..
            })
        ));
        assert_eq!(
            fs::read(&source).expect("restored original bytes"),
            b"moved bytes"
        );
        assert_eq!(fs::read_dir(folder).expect("receiving contents").count(), 0);
        assert!(matches!(
            b.compensate(Command::Undo { operation }).await,
            Ok(Reply::Settled {
                completed: 1,
                error: None,
                undone: true,
                ..
            })
        ));
    });
}

#[test]
fn clipboard_bridge_current_viewer_can_copy_but_cannot_cut_or_replace_offer() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let connection = fixture.connect("viewer-window").await;
        let path = fixture.directory.path().join("source");
        fs::write(&path, b"viewer readable bytes").expect("source");
        let ticket = connection.capture(&path, false).await;
        {
            let mut tree = fixture.tree.write();
            let workspace = tree.workspaces.first_mut().expect("fixture workspace");
            let principal = workspace.policy.owner.take().expect("original owner");
            workspace.admit(principal, tilo_action_policy::Role::Viewer);
            drop(tree);
        }
        let offer = connection.publish(ticket.clone(), false).await;
        let error = connection
            .command(Command::Publish {
                sources: vec![ticket],
                cut: true,
            })
            .await
            .expect_err("current Viewer cannot Cut even with a writable app grant");
        assert!(error.contains("required permission"), "{error}");
        let Reply::Snapshot {
            offer: Some(current),
        } = connection
            .command(Command::Snapshot)
            .await
            .expect("Viewer reads clipboard")
        else {
            panic!("Copy offer retained");
        };
        assert_eq!(current.id, offer);
        assert!(!current.cut);
        assert_eq!(
            fs::read(path).expect("source untouched"),
            b"viewer readable bytes"
        );
    });
}

#[test]
fn clipboard_bridge_same_process_connection_cannot_spend_another_routes_ticket() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let connection = fixture.connect("window-a").await;
        let path = fixture.directory.path().join("source");
        fs::write(&path, b"retained source").expect("source");
        let ticket = connection.capture(&path, false).await;
        let route = tilo_action_broker::route::CrdtRoute::record(APP, &fixture.room, "window-b");
        let other = connection
            .client
            .join_channel(&route.channel_key(), &fixture.identity)
            .await
            .expect("another route on the same actual peer connection");
        let error = other
            .local_file_clipboard_command(
                &Command::Publish {
                    sources: vec![ticket.clone()],
                    cut: false,
                },
                None,
            )
            .await
            .expect_err("same peer cannot spend another route's ticket");
        assert!(
            error.to_string().contains("another Files window"),
            "{error}"
        );
        let offer = connection.publish(ticket, false).await;
        let Reply::Snapshot {
            offer: Some(current),
        } = connection
            .command(Command::Snapshot)
            .await
            .expect("original route's offer")
        else {
            panic!("original offer");
        };
        assert_eq!(current.id, offer);
    });
}

#[test]
fn clipboard_bridge_local_home_consent_refuses_foreign_person_without_blocking_shared_grants() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let system = home.config().join("test-system");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
        .set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-tilo-test-bus",
        )
        .remove("TILO_USER_IDENTITY")
        .remove("TILO_IDENTITY_BROKER_FD");
    let identity = runtime_identity::identity_dir_from_env().unwrap();
    crate::firstboot::ensure_throwaway_test_identity(
        &identity,
        &crate::firstboot::collaboration_identity_paths(),
    )
    .unwrap();
    let fixture = Fixture::new();
    let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
    let identity_id = registry
        .load_reconciled()
        .unwrap()
        .default_identity
        .unwrap();
    registry
        .bind_workspace(&fixture.room, &identity_id)
        .unwrap();
    let workspace = fixture.tree.read().workspaces.first().unwrap().id;
    assert!(
        crate::shell::search::providers::applications::grant_file_manager_home(
            &mut fixture.tree.write(),
            workspace,
            APP,
        )
    );
    let private = home.home().join("private.txt");
    fs::write(&private, b"selected person's private bytes").unwrap();
    let public = fixture.directory.path().join("shared.txt");
    let receiving = fixture.directory.path().join("receiving");
    fs::write(&public, b"explicitly shared bytes").unwrap();
    fs::create_dir(&receiving).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let connection = fixture.connect("foreign-person").await;
        let (retained, descriptor) = selected(&private);
        let error = connection
            .command(Command::Capture {
                sources: vec![descriptor],
                destination: None,
            })
            .await
            .expect_err("an admitted different person cannot inherit local home consent");
        assert!(error.contains("live grants"), "{error}");
        drop(retained);
        let ticket = connection.capture(&public, false).await;
        let offer = connection.publish(ticket, false).await;
        let destination = connection.capture(&receiving, true).await;
        let operation = uuid::Uuid::new_v4().to_string();
        connection
            .command(Command::Paste {
                offer,
                destination,
                operation: operation.clone(),
                conflict: runtime_collab::file_clipboard::Conflict::KeepBoth,
            })
            .await
            .unwrap();
        assert!(matches!(
            connection.settled(&operation).await,
            Reply::Settled {
                completed: 1,
                error: None,
                ..
            }
        ));
    });
    assert_eq!(
        fs::read(private).unwrap(),
        b"selected person's private bytes"
    );
    assert_eq!(
        fs::read(receiving.join("shared.txt")).unwrap(),
        b"explicitly shared bytes"
    );
}

/// Capture `names` of `folder` by name, `chunk` entries per command.
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on this test's LocalSet"
)]
async fn capture_named(
    a: &Connection,
    folder: &Path,
    names: &[String],
    chunk: usize,
) -> Vec<String> {
    use runtime_collab::file_clipboard::NamedEntry;
    let (_folder, descriptor) = selected(folder);
    let entries: Vec<NamedEntry> = names
        .iter()
        .map(|name| {
            let metadata = fs::symlink_metadata(folder.join(name)).expect("shown entry");
            NamedEntry {
                name: name.as_bytes().to_vec(),
                expected: ObjectIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            }
        })
        .collect();
    let mut tickets = Vec::new();
    for entries in entries.chunks(chunk) {
        let Reply::CapturedNamed { ticket, count } = a
            .command(Command::CaptureNamed {
                folder: descriptor.clone(),
                entries: entries.to_vec(),
            })
            .await
            .expect("capture by name")
        else {
            panic!("named ticket");
        };
        assert_eq!(count, entries.len());
        tickets.push(ticket);
    }
    tickets
}

/// Paste `offer` into `dest` with `conflict`: the operation and the answer.
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto runs on this test's LocalSet"
)]
async fn paste_into(
    a: &Connection,
    offer: &str,
    dest: &Path,
    conflict: runtime_collab::file_clipboard::Conflict,
) -> (String, Reply) {
    let (_held, descriptor) = selected(dest);
    let Reply::Captured {
        destination: Some(destination),
        ..
    } = a
        .command(Command::Capture {
            sources: vec![],
            destination: Some(descriptor),
        })
        .await
        .expect("destination ticket")
    else {
        panic!("destination");
    };
    let operation = uuid::Uuid::new_v4().to_string();
    let reply = a
        .command(Command::Paste {
            offer: offer.into(),
            destination,
            operation: operation.clone(),
            conflict,
        })
        .await
        .expect("paste answer");
    (operation, reply)
}

/// A folder's entries captured by name: hundreds of them cost the host one
/// descriptor for the folder, a paste into a folder that has some of the
/// names asks first, Replace sends the existing item to Trash, and Undo
/// puts it back; Skip leaves it.
#[test]
fn clipboard_bridge_copies_a_large_named_selection_and_settles_clashes() {
    use runtime_collab::file_clipboard::Conflict;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fixture = Fixture::new();
        let a = fixture.connect("window-a").await;
        let many = fixture.directory.path().join("many");
        fs::create_dir(&many).expect("source folder");
        let names: Vec<String> = (0..300).map(|i| format!("f{i:03}.txt")).collect();
        for name in &names {
            fs::write(many.join(name), b"new").expect("source entry");
        }
        let tickets = capture_named(&a, &many, &names, 150).await;
        let reserved = fixture
            .clipboard
            .state
            .lock()
            .unwrap()
            .reserved_descriptors();
        assert!(reserved < 16, "300 entries hold {reserved} descriptors");
        let Reply::Published { offer } = a
            .command(Command::Publish {
                sources: tickets,
                cut: false,
            })
            .await
            .expect("publish the named entries")
        else {
            panic!("offer");
        };
        assert_eq!(offer.count, 300);

        let dest = fixture.directory.path().join("dest");
        fs::create_dir(&dest).expect("destination");
        fs::write(dest.join("f000.txt"), b"old").expect("clashing item");
        let (_, asked) = paste_into(&a, &offer.id, &dest, Conflict::Ask).await;
        assert_eq!(
            asked,
            Reply::Conflicts {
                names: vec!["f000.txt".into()],
                count: 1
            }
        );
        assert_eq!(
            fs::read_dir(&dest).unwrap().count(),
            1,
            "nothing pasted yet"
        );

        let (operation, running) = paste_into(&a, &offer.id, &dest, Conflict::Replace).await;
        assert!(matches!(running, Reply::Running { .. }), "{running:?}");
        assert!(matches!(
            a.settled(&operation).await,
            Reply::Settled {
                completed: 300,
                error: None,
                ..
            }
        ));
        assert_eq!(fs::read(dest.join("f000.txt")).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 300, "no “f000 2.txt”");
        let undone = a
            .compensate(Command::Undo { operation })
            .await
            .expect("undo receipt");
        assert!(
            matches!(
                undone,
                Reply::Settled {
                    error: None,
                    undone: true,
                    ..
                }
            ),
            "{undone:?}"
        );
        assert_eq!(
            fs::read(dest.join("f000.txt")).unwrap(),
            b"old",
            "the replaced item is back"
        );
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);

        let skipped = fixture.directory.path().join("skipped");
        fs::create_dir(&skipped).expect("second destination");
        fs::write(skipped.join("f001.txt"), b"keep").expect("clashing item");
        let (operation, _) = paste_into(&a, &offer.id, &skipped, Conflict::Skip).await;
        assert!(matches!(
            a.settled(&operation).await,
            Reply::Settled {
                completed: 299,
                error: None,
                ..
            }
        ));
        assert_eq!(fs::read(skipped.join("f001.txt")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&skipped).unwrap().count(), 300);
    });
}
