//! The original native governor ledger on the existing certified action lane.
//! This fixture supplies synthetic key custody out of band; it never replaces
//! governor commits with transport acknowledgements or DTO mutation.
use super::*;
use std::os::unix::fs::PermissionsExt as _;
use tilo_peer_state::owner_commit::{CommitFuture, OwnerCommitRequest, OwnerCommitService};

pub(super) struct Attached {
    pub owner: NativeWorkspaceOwner,
    broker: tilo_action_broker::collab_broker::BrokerHandle,
    task: tokio::task::JoinHandle<()>,
    unused_documents: Arc<tilo_action_broker::collab_outbound::recording::RecordingSink>,
    _runtime: tempfile::TempDir,
}

impl Attached {
    pub async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
        assert!(
            self.unused_documents.drain().is_empty(),
            "receipt fixture must publish only the original native outbox"
        );
        drop(self.broker);
    }
}

struct Authority(NativeWorkspaceOwner);

impl tilo_action_broker::peer_actions::CertificateAuthority for Authority {
    fn authorize(&self, room: uuid::Uuid, certificate: &DeviceCert) -> Result<(), String> {
        self.0.with_current(
            RoomId::parse(&room.to_string()).ok_or("invalid receipt room")?,
            |view| {
                let now = SystemTime::now();
                if !view.peer().certificates().contains(certificate) {
                    return Err("receipt certificate is not current native admission".into());
                }
                view.peer()
                    .permissions(certificate.device_public_key, now)
                    .map(|_| ())
            },
        )
    }
}

struct Governor {
    owner: NativeWorkspaceOwner,
    key: ed25519_dalek::SigningKey,
    certificate: DeviceCert,
    authority: ActualAuthority,
    publisher: Arc<tilo_relay::RelayCoeditSession>,
}

impl OwnerCommitService for Governor {
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }

    fn commit(&self, _: OwnerCommitRequest) -> CommitFuture {
        Box::pin(async { Err("fixture serves only authenticated member audits".into()) })
    }

    fn commit_member_effect_receipt(
        &self,
        request: OwnerCommitRequest,
        caller: DeviceCert,
    ) -> CommitFuture {
        let result = (|| {
            let receipt = self.owner.commit_member_effect_receipt(
                &request,
                &caller,
                &self.certificate,
                &self.key,
                SystemTime::now(),
            )?;
            let room = RoomId::parse(&request.room).ok_or("invalid receipt room")?;
            let frames = self
                .owner
                .with_outgoing(room, SystemTime::now(), |_, outgoing| {
                    Ok(outgoing
                        .entries()
                        .map(|entry| entry.bytes().to_vec())
                        .collect::<Vec<_>>())
                })?;
            let mut state = self
                .authority
                .lock()
                .map_err(|_| "governor fixture poisoned")?;
            let state = state.as_mut().ok_or("governor fixture unavailable")?;
            for frame in frames {
                state.receive(
                    tilo_peer_state::TransportOrigin::RetainedNativePublication {
                        publisher: self.certificate.device_public_key,
                    },
                    postcard::from_bytes(&frame).map_err(|error| error.to_string())?,
                )?;
            }
            state.refresh_checkpoint()?;
            super::refresh::publish_committed(state, &self.publisher);
            Ok(receipt)
        })();
        Box::pin(async move { result })
    }
}

pub(super) async fn attach(
    directory: &std::path::Path,
    authority: ActualAuthority,
    publisher: Arc<tilo_relay::RelayCoeditSession>,
) -> Attached {
    let path = directory.join("source-state.json");
    let mut tree = crate::shell::persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    let tree = tree.shared();
    let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
    let (key, certificate, frames) = {
        let state = authority.lock().unwrap();
        let state = state.as_ref().unwrap();
        (
            state.governor_key().clone(),
            state.governor().clone(),
            state
                .outgoing()
                .map(|(bytes, _)| bytes.to_vec())
                .collect::<Vec<_>>(),
        )
    };
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    for frame in frames {
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: certificate.device_public_key,
                },
                postcard::from_bytes(&frame).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
    }
    let service = Arc::new(Governor {
        owner: owner.clone(),
        key,
        certificate,
        authority,
        publisher: publisher.clone(),
    });
    let unused_documents =
        Arc::new(tilo_action_broker::collab_outbound::recording::RecordingSink::default());
    let outbound = tilo_action_broker::collab_outbound::PeerPublisher::with_owner_commit(
        unused_documents.clone(),
        service,
    );
    // This receipt-only broker opens no app document routes. Its governor
    // publishes the actual committed native outbox through the coedit session.
    let runtime = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(directory)
        .unwrap();
    let broker = tilo_action_broker::collab_broker::spawn_native_peer_with_services(
        runtime.path().join("receipt-broker.sock"),
        Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
            tree,
        )),
        outbound,
        tilo_action_broker::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.join("receipt-journal"),
            certificate_authority: Arc::new(Authority(owner.clone())),
        },
        tilo_action_broker::services::HostServices::default(),
    )
    .unwrap();
    let actions = broker.app_action_broker().unwrap();
    let (queue, host) = actions.peer_action_attachment().unwrap();
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let result = queue
            .serve_with_readiness(
                publisher.clone(),
                uuid::Uuid::parse_str(tilo_joined_room::testing::ROOM).unwrap(),
                host,
                ready_tx,
            )
            .await;
        publisher.close().await;
        result.unwrap();
    });
    tilo_test_env::within(
        "the certified native effect broker to acknowledge registration",
        async {
            while !*ready_rx.borrow_and_update() {
                ready_rx
                    .changed()
                    .await
                    .expect("native effect broker registration ended");
            }
        },
    )
    .await;
    Attached {
        owner,
        broker,
        task,
        unused_documents,
        _runtime: runtime,
    }
}
