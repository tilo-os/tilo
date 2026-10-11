//! New isolated original-package/kernel-custody broker journey, not physical delivery.
use super::*;
use crate::{
    RuntimeServices as _,
    standalone::{self, Certificates, ProfileWorkspace, Standalone},
};
use automerge::{ReadDoc, transaction::Transactable};
use ed25519_dalek::Signer as _;
use runtime_collab::committed_document::{CreationChunk, Reply, Request};
use std::os::fd::AsRawFd as _;
// Capnp clients stay on the current-thread LocalSet used by this fixture.
type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
use tilo_action_broker::{
    collab_broker::{self, CrdtRoute, FanoutState},
    collab_core::BrokerChannels,
    collab_outbound::PeerPublisher,
    services::{HostServices, LauncherServices, WorkspaceAuthority},
};
const TEST: &str = "standalone::committed_documents::tests::original_launch_creates_initializes_reopens_and_retired_runtime_refuses_reads";
const APP: &str = "so.tilo.viewer";
const KIND: &str = "viewer.document";
const RECORD: &str = "new-deterministic-sdk-record";

fn chunk(reply: Reply) -> CreationChunk {
    CreationChunk::from_reply(reply).unwrap()
}
fn acknowledge<'a>(
    channel: &'a runtime_collab::ChannelHandle,
    chunk: &'a CreationChunk,
) -> LocalFuture<'a, ()> {
    Box::pin(async move {
        assert_eq!(
            channel
                .committed_document(&Request::Cancel {
                    capture: chunk.capture,
                    token: chunk.token
                })
                .await
                .unwrap(),
            Reply::Cancelled
        );
    })
}

fn start_broker(
    profile_root: &std::path::Path,
    workspace: Arc<ProfileWorkspace>,
    apps: &Arc<crate::apps::Apps>,
    joined: &Arc<standalone::Joined>,
) -> (
    collab_broker::BrokerHandle,
    tilo_action_broker::peer_actions::PolicyOrigin,
) {
    let room = workspace.room();
    let owner = standalone::room_owner_services(
        profile_root,
        &room,
        &workspace,
        apps,
        false,
        tilo_action_broker::channel_encryption::ChannelKeyProvider::default(),
    )
    .unwrap();
    let (_, certificate) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
    let (origin, time) = standalone::execution_policy_clock().unwrap();
    let native = Arc::new(Documents {
        own: owner.documents,
        joined: joined.clone(),
    });
    let broker = collab_broker::spawn_with_services(
        profile_root.join("sdk-read.sock"),
        Some(workspace as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_optional_owner_commit(None, owner.commits),
        None,
        HostServices {
            committed_documents: Some(native),
            native_certificates: Some(Arc::new(Certificates {
                room,
                device: certificate.device_public_key,
                policy_time: time,
                joined: Arc::downgrade(joined),
            })),
            launcher: Some(apps.clone() as Arc<dyn LauncherServices>),
            ..HostServices::default()
        },
        Arc::new(std::sync::Mutex::new(BrokerChannels::default())),
        Arc::new(FanoutState::new()),
    )
    .unwrap();
    (broker, origin)
}
struct Fixture {
    profile_root: std::path::PathBuf,
    room: String,
    client: runtime_collab::CollabClient,
    presentation: Vec<u8>,
    _environment: tilo_test_env::ScopedEnv,
    stream: std::os::unix::net::UnixStream,
}
impl Fixture {
    fn connect(profile_root: &std::path::Path, broker: &collab_broker::BrokerHandle) -> Self {
        let room = ProfileWorkspace::open(profile_root).unwrap().room();
        let (key, certificate) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
        let mut stream = std::os::unix::net::UnixStream::connect(broker.capnp_socket()).unwrap();
        launcher::services::write_collab_handoff(&mut stream, APP, &room).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_SVC_COLLAB_FD", stream.as_raw_fd().to_string());
        let client = runtime_collab::try_connect().unwrap();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        Self {
            profile_root: profile_root.to_path_buf(),
            room,
            client,
            presentation,
            _environment: environment,
            stream,
        }
    }
    // Keep the retired inherited descriptor's slot occupied until the new
    // upstream is installed; the portal hub is process-owned by that slot.
    fn retire(self) -> std::os::unix::net::UnixStream {
        self.stream
    }
    fn channel<'a>(
        &'a self,
        record: &'a str,
        kind: &'a str,
    ) -> LocalFuture<'a, runtime_collab::ChannelHandle> {
        Box::pin(async move {
            self.client
                .join_channel_as_kind(
                    &CrdtRoute::record(APP, &self.room, record).channel_key(),
                    &self.presentation,
                    "",
                    "person",
                    kind,
                )
                .await
                .unwrap()
        })
    }
    fn owner_bytes(&self) -> Vec<u8> {
        std::fs::read(self.profile_root.join(standalone::owner::ROOM_OWNER)).unwrap()
    }
    fn create_and_initialize(&self) -> LocalFuture<'_, [u8; 32]> {
        Box::pin(async move {
            let channel = self.channel(RECORD, KIND).await;
            let Reply::Created {
                receipt,
                chunk: empty,
            } = channel
                .committed_document(&Request::Open {
                    known_revision: None,
                })
                .await
                .unwrap()
            else {
                panic!("original signed launch must receive a creation receipt");
            };
            let mut doc = automerge::AutoCommit::load(&empty.bytes).unwrap();
            assert_eq!(doc.keys(automerge::ROOT).count(), 0, "apps start empty");
            assert_eq!(receipt.workspace.id, self.room);
            acknowledge(&channel, &empty).await;
            doc.put(automerge::ROOT, "unknown_future_field", "retained")
                .unwrap();
            let bootstrap = doc.save();
            let before = self.owner_bytes();
            assert!(
                channel
                    .committed_document(&Request::Initialize {
                        token: [0; 16],
                        revision: empty.revision,
                        kind: KIND.into(),
                        document: bootstrap.clone()
                    })
                    .await
                    .is_err()
            );
            assert_eq!(self.owner_bytes(), before);
            let request = Request::Initialize {
                token: receipt.token,
                revision: empty.revision,
                kind: KIND.into(),
                document: bootstrap,
            };
            let Reply::Initialized { revision } =
                channel.committed_document(&request).await.unwrap()
            else {
                panic!("bootstrap was not durably initialized");
            };
            assert_eq!(
                channel.committed_document(&request).await.unwrap(),
                Reply::Initialized { revision }
            );
            revision
        })
    }
    fn refuse_untyped_creation(&self) -> LocalFuture<'_, ()> {
        Box::pin(async move {
            let channel = self.channel("untyped-independent-app-record", "").await;
            let before = self.owner_bytes();
            let refused = channel
                .committed_document(&Request::Open {
                    known_revision: None,
                })
                .await
                .unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains("application creation requires SDK resource kind"),
                "original signed launch custody alone cannot invent a resource kind: {refused}"
            );
            assert_eq!(
                self.owner_bytes(),
                before,
                "an untyped app channel must not obtain a creation receipt or mutate the owner"
            );
        })
    }

    fn reopen_and_refuse<'a>(
        &'a self,
        revision: [u8; 32],
        origin: &'a tilo_action_broker::peer_actions::PolicyOrigin,
    ) -> LocalFuture<'a, ()> {
        Box::pin(async move {
            let channel = self.channel(RECORD, KIND).await;
            let read = chunk(
                channel
                    .committed_document(&Request::Open {
                        known_revision: None,
                    })
                    .await
                    .unwrap(),
            );
            assert_eq!(read.revision, revision);
            let restored = automerge::AutoCommit::load(&read.bytes).unwrap();
            assert_eq!(
                restored
                    .get(automerge::ROOT, "unknown_future_field")
                    .unwrap()
                    .unwrap()
                    .0
                    .into_string()
                    .unwrap(),
                "retained"
            );
            acknowledge(&channel, &read).await;
            assert_eq!(
                channel
                    .committed_document(&Request::Open {
                        known_revision: Some(revision)
                    })
                    .await
                    .unwrap(),
                Reply::Unchanged { revision }
            );
            let refused = self
                .channel("undeclared-new-record", "undeclared.kind")
                .await;
            let before = self.owner_bytes();
            assert!(
                refused
                    .committed_document(&Request::Open {
                        known_revision: None
                    })
                    .await
                    .is_err()
            );
            assert_eq!(self.owner_bytes(), before);
            origin.invalidate();
            assert!(
                channel
                    .committed_document(&Request::Open {
                        known_revision: None
                    })
                    .await
                    .is_err(),
                "original runtime custody ended"
            );
            assert_eq!(self.owner_bytes(), before);
        })
    }
}

fn journey(profile_root: &std::path::Path) -> LocalFuture<'_, ()> {
    Box::pin(async move {
        let apps = crate::apps::launched_app_fixture(
            profile_root,
            &ProfileWorkspace::open(profile_root).unwrap().room(),
        );
        let (joined, _directory) = standalone::Joined::test_empty().await;
        let (broker, origin) = start_broker(
            profile_root,
            Arc::new(ProfileWorkspace::open(profile_root).unwrap()),
            &apps,
            &joined,
        );
        let first = Fixture::connect(profile_root, &broker);
        first.refuse_untyped_creation().await;
        let revision = first.create_and_initialize().await;
        let retired_stream = first.retire();
        drop(broker);
        origin.invalidate();
        // Reopen the actual durable owner and broker, retaining the same original launch.
        let (broker, origin) = start_broker(
            profile_root,
            Arc::new(ProfileWorkspace::open(profile_root).unwrap()),
            &apps,
            &joined,
        );
        let restarted = Fixture::connect(profile_root, &broker);
        restarted.reopen_and_refuse(revision, &origin).await;
        drop(restarted);
        drop(broker);
        origin.invalidate();
        drop(retired_stream);
        joined.endpoint().close().await;
    })
}
#[test]
fn original_launch_creates_initializes_reopens_and_retired_runtime_refuses_reads() {
    let Some(profile_root) = standalone::tests::isolated_root(TEST) else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    // Provision through the existing real standalone startup, in this private child.
    drop(Standalone::start_reporting(&profile_root, &mut |_| {}).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, journey(&profile_root));
}

const INDEPENDENT_TEST: &str = "standalone::committed_documents::tests::independent_sdk_launch_without_original_handoff_is_refused_before_owner_mutation";
const INDEPENDENT_HELPER: &str = "TILO_INDEPENDENT_SDK_CUSTODY_HELPER";

#[test]
fn independent_sdk_launch_without_original_handoff_is_refused_before_owner_mutation() {
    if std::env::var_os(INDEPENDENT_HELPER).is_some() {
        // This negative case requests native document custody. The untyped
        // SDK constructor is also used by legitimate directory publishers.
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file_record(uuid::Uuid::nil()),
            ui::record::RecordKind::new("independent.document"),
        );
        let mut manager = ui::collaboration::CollaborationManager::for_native_record(&checkout);
        let started = std::time::Instant::now();
        while manager.state() == ui::collaboration::CollaborationState::Connecting
            && started.elapsed() < tilo_test_env::hang_bound()
        {
            let _ = manager.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !manager.is_connected(),
            "a raw independent descriptor must not acquire launcher custody"
        );
        assert!(
            format!("{:?}", manager.state()).contains("committed read requires launcher custody"),
            "refusal must report the missing original producer: {:?}",
            manager.state()
        );
        return;
    }
    let Some(profile_root) = standalone::tests::isolated_root(INDEPENDENT_TEST) else {
        return;
    };
    let session = Standalone::start_reporting(&profile_root, &mut |_| {}).unwrap();
    let owner_path = profile_root.join(standalone::owner::ROOM_OWNER);
    let before = std::fs::read(&owner_path).unwrap();
    let log_path = profile_root.join("independent-custody-child.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", INDEPENDENT_TEST, "--nocapture"])
        .env(INDEPENDENT_HELPER, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    // Exercise the raw inherited transport without the signed launch producer.
    let mut child = crate::unix::launch(
        command,
        "dev.tilo.independent-custody",
        session.room(),
        &crate::unix::ServiceSockets {
            collab: session.broker.capnp_socket(),
            identity: &profile_root.join(crate::IDENTITY_SOCKET),
            blobs: &profile_root.join(crate::BLOB_SOCKET),
        },
    )
    .unwrap();
    let status = child.wait().unwrap();
    // Keep the real host serving until the independently executing child exits.
    drop(session);
    let report = std::fs::read_to_string(log_path).unwrap();
    assert!(
        status.success(),
        "actual independently launched SDK child: {report}"
    );
    assert_eq!(
        std::fs::read(owner_path).unwrap(),
        before,
        "failed closed-context acquisition must not create or rewrite owner state"
    );
}

#[path = "independent_sdk_tests.rs"]
mod independent_sdk;

#[test]
fn registered_independent_source_cannot_downgrade_to_creator_only_handoff() {
    const TEST: &str = "standalone::committed_documents::tests::registered_independent_source_cannot_downgrade_to_creator_only_handoff";
    let Some(root) = standalone::tests::isolated_root(TEST) else {
        return;
    };
    let store = crate::apps::independent_source_fixture(&root, APP, KIND);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", store);
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    session
        .apps
        .register_independent_launch(
            std::process::id(),
            APP,
            session.room(),
            &std::env::current_exe().unwrap(),
        )
        .unwrap();
    let before = std::fs::read(root.join(standalone::owner::ROOM_OWNER)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        // A real inherited socket with valid signed source, but deliberately
        // choosing the creator-only handoff instead of executing proof.
        let fixture = Fixture::connect(&root, &session.broker);
        let result = fixture
            .client
            .join_channel_as_kind(
                &CrdtRoute::record(APP, &fixture.room, RECORD).channel_key(),
                &fixture.presentation,
                "",
                "person",
                KIND,
            )
            .await;
        let Err(error) = result else {
            panic!("creator-only handoff acquired independent executing custody");
        };
        assert!(
            error
                .to_string()
                .contains("independent launch requires its original executing-peer binding"),
            "{error}"
        );
    });
    assert_eq!(
        std::fs::read(root.join(standalone::owner::ROOM_OWNER)).unwrap(),
        before,
        "handoff downgrade must not create or rewrite durable owner state"
    );
    drop(session);
}
