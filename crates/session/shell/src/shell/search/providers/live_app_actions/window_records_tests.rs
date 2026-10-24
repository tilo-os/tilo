//! One app window holding two records, through the production broker: a
//! launcher-attested connection joins both and publishes on each. Search
//! shows each record's actions once, under that record's title and the app's
//! name and icon. A record the window drops leaves the window, and its
//! actions read exactly as before: only opening them now opens the app.
use std::sync::{Arc, Mutex};

use automerge::AutoCommit;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use super::{LiveAppActionsProvider, PROVIDER_ID};
use crate::shell::chrome_state::ChromeState;
use crate::shell::search::{ResourceKind, SearchBudget, SearchContext, SearchProvider as _};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};

const APP: &str = "so.tilo.notes";

struct Broker {
    _directory: tempfile::TempDir,
    socket: std::path::PathBuf,
    room: String,
    identity: Vec<u8>,
    tree: WorkspaceTreeHandle,
    handle: BrokerHandle,
}

impl Broker {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("private broker directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("owner-only broker socket directory");
        let socket = directory.path().join("broker.sock");
        let signing_root = SigningKey::from_bytes(&[131; 32]);
        let device = SigningKey::from_bytes(&[132; 32]);
        let mut workspace = Workspace::new(WorkspaceId(131), "Window records fixture");
        workspace.set_owner(
            tilo_action_broker::collab_broker_capnp::principal_id_for_root(
                signing_root.verifying_key().to_bytes(),
            ),
        );
        let room = workspace.room().to_string();
        let mut tree = WorkspaceTree {
            persist_path: Some(directory.path().join("state.json")),
            ..Default::default()
        };
        tree.workspaces.push(workspace);
        tree.set_active(WorkspaceId(131));
        let tree = tree.shared();
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: signing_root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Window records fixture device".into(),
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
        let mut services =
            crate::shell::broker_services::native_services(Some(tree.clone()), None, None);
        // The fixture device is enrolled through the broker's test-support
        // configuration; launcher admission and workspace policy stay the
        // production implementations.
        services.native_certificates = None;
        let handle = tilo_action_broker::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(Arc::new(
                crate::shell::broker_services::NativeWorkspaceAuthority(tree.clone()),
            )),
            tilo_action_broker::collab_outbound::PeerPublisher::disabled(),
            None,
            services,
            Some(Arc::new(vec![device.verifying_key().to_bytes()])),
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("production broker with an enrolled fixture device");
        tilo_test_env::wait_until("window records broker socket", || socket.exists());
        Self {
            _directory: directory,
            socket,
            room,
            identity,
            tree,
            handle,
        }
    }

    /// The app window: one launcher-attested connection.
    async fn window(&self) -> (runtime_collab::CollabClient, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt as _;
        crate::shell::launcher::register_in_process_launch(APP, &self.room);
        let mut stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .expect("local socket");
        let handoff = serde_json::to_vec(&serde_json::json!({
            "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": APP, "workspace_id": self.room,
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
        );
        let driver = tokio::task::spawn_local(async move {
            let _ = rpc.await;
        });
        (client, driver)
    }

    /// The window opens `record` and publishes its one action, titled for it.
    #[expect(
        clippy::future_not_send,
        reason = "Cap'n Proto runs on this test's LocalSet"
    )]
    async fn open(
        &self,
        window: &runtime_collab::CollabClient,
        record: &str,
        title: &str,
    ) -> runtime_collab::ChannelHandle {
        let route = tilo_action_broker::route::CrdtRoute::record(APP, &self.room, record);
        let channel = window
            .join_channel(&route.channel_key(), &self.identity)
            .await
            .expect("the window joins its record");
        let action = runtime_collab::ExportedAction {
            resource: format!("note:{record}"),
            semantic_identity: None,
            action: "rename".into(),
            resource_kind: "note".into(),
            resource_label: title.into(),
            action_label: format!("Rename {title}"),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract::default(),
            view: None,
            input: runtime_collab::ExportedInput::None,
        };
        channel
            .publish_actions(APP, &[action])
            .await
            .expect("the window publishes for its record");
        channel
    }
}

/// What search shows for the fixture app now: each row's title, secondary
/// line and whether it wears the app's icon.
fn shown(broker: &Broker, provider: &LiveAppActionsProvider) -> Vec<(String, Shown)> {
    let chrome = ChromeState::new_with_handle(broker.tree.clone());
    let chrome = chrome.borrow();
    let doc = AutoCommit::new();
    let ctx = SearchContext {
        query_scope: Some(crate::shell::chrome_state::SearchScope::All),
        include_programmatic: false,
        needle: "rename",
        wanted: &[ResourceKind::Action],
        chrome: &chrome,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };
    let app = broker.handle.app_action_broker().expect("live broker");
    *provider.surface_snapshot.borrow_mut() = Some(LiveAppActionsProvider::build_local_snapshot(
        &app,
        &LiveAppActionsProvider::build_scope(&ctx),
        &provider.build_probe,
    ));
    let mut shown: Vec<_> = provider
        .query_page(ctx, None, SearchBudget::PALETTE)
        .results
        .into_iter()
        .filter(|row| row.provider_id == PROVIDER_ID && row.id.starts_with(APP))
        .map(|row| {
            let icon = provider.row_icon(&row) == Some(ui::icons::app_glyph(APP, None));
            (
                row.title,
                Shown {
                    subtitle: row.subtitle,
                    facts: row.facts,
                    icon,
                },
            )
        })
        .collect();
    shown.sort();
    shown
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Shown {
    subtitle: Option<String>,
    facts: Vec<String>,
    icon: bool,
}

/// A record's row as a person reads it: its action, which names the
/// record, over the app's name, under the app's icon. No package id, no
/// mechanism, and the record not named twice.
fn record_row(action: &str) -> (String, Shown) {
    (
        action.to_owned(),
        Shown {
            subtitle: None,
            facts: vec!["Notes".to_owned()],
            icon: true,
        },
    )
}

#[test]
fn a_window_holding_two_records_shows_each_once_and_withdraws_the_one_it_closes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let broker = Broker::new();
        let provider =
            LiveAppActionsProvider::new(broker.handle.app_action_broker().expect("live broker"));
        let (window, driver) = broker.window().await;
        let groceries = broker.open(&window, "groceries", "Groceries").await;
        let trip = broker.open(&window, "trip", "Trip plan").await;

        let both = vec![
            record_row("Rename Groceries"),
            record_row("Rename Trip plan"),
        ];
        assert_eq!(
            shown(&broker, &provider),
            both,
            "each record the window holds is shown once, under its own title"
        );

        // The window closes the trip record and keeps the groceries one.
        drop(trip);
        let app = broker.handle.app_action_broker().expect("live broker");
        let deadline = tilo_test_env::hang_deadline();
        while app
            .surfaces()
            .iter()
            .any(|surface| surface.live && surface.route.record_id == "trip")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the closed record stays published"
            );
            tokio::task::yield_now().await;
        }
        let after = shown(&broker, &provider);
        drop(provider);
        assert!(
            app.surfaces()
                .iter()
                .any(|surface| surface.live && surface.route.record_id == "groceries"),
            "the record the window keeps stays open in it"
        );
        drop(app);
        // The closed record still exists, so it stays findable, and reads
        // the same as an open one: opening it reopens the app on it.
        assert_eq!(after, both, "a closed record's row reads as an open one's");
        drop((groceries, window));
        driver.abort();
    });
}
