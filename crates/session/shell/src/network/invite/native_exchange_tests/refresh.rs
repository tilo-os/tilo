//! Real-clock restart regression; no fresh checkpoint is injected manually.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use super::*;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};

/// The parent exchange test's state directory, device identity, endpoint,
/// relay and publishing session, still live when its restart phase begins.
pub(super) struct LiveExchange<'a> {
    pub directory: &'a tempfile::TempDir,
    pub identity: &'a HostIdentity,
    pub endpoint: &'a iroh::Endpoint,
    pub relay: &'a tilo_relay::Relay,
    pub publisher: &'a tilo_relay::RelayCoeditSession,
}

#[allow(
    clippy::too_many_lines,
    reason = "continuation of the parent exchange test: it takes over that test's live relay, \
              endpoints and custody and runs the restart phase as one linear scenario"
)]
pub(super) async fn restart_after_offline_snapshot(
    live: LiveExchange<'_>,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    authority: ActualAuthority,
    receiving_custody: Arc<Mutex<Option<NativeWorkspaceOwner>>>,
) {
    let LiveExchange {
        directory,
        identity,
        endpoint,
        relay,
        publisher,
    } = live;
    let now = SystemTime::now();
    let topic = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    // The normal member producer signs its Snapshot before admission. Only the
    // newly prepared draft is changed; retained original bytes are never edited.
    let (request, original, expected_heads) = owner
        .with_current(room, |view| {
            let mut doc =
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap()).unwrap();
            doc.set_actor(automerge::ActorId::from(
                identity
                    .device_signing_key
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ));
            let heads = doc.get_heads();
            doc.put(ROOT, "real-clock-offline-edit", "retained across restart")
                .unwrap();
            doc.commit();
            let expected = doc.get_heads();
            let mut message = view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&heads),
                &identity.device_signing_key,
                now,
            )?;
            let NeighbourMsg::AppDocument {
                app_id,
                record_id,
                envelope,
                ..
            } = &mut message
            else {
                unreachable!()
            };
            envelope.payload = collab_types::DocumentMutation::Snapshot(doc.save())
                .encode()
                .unwrap();
            collab_types::sign_op(envelope, &identity.device_signing_key).unwrap();
            let request = OwnerCommitRequest {
                room: room.to_string(),
                operation: CommitOperation::Document {
                    app: app_id.clone(),
                    record: record_id.clone(),
                    envelope: envelope.clone(),
                },
            };
            Ok((request, postcard::to_allocvec(&message).unwrap(), expected))
        })
        .unwrap();
    owner.commit_document(request, now).unwrap();
    owner
        .reconcile_outgoing(room, &identity.device_signing_key, now)
        .unwrap();
    assert_eq!(
        owner
            .with_outgoing(room, now, |_, outgoing| Ok(outgoing.entries().count()))
            .unwrap(),
        1,
        "real member Snapshot must have one lawful Incremental projection before restart"
    );
    let floors = owner
        .with_current(room, |view| Ok(view.peer().replay_floors()))
        .unwrap();
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 2);

    // Retire completed enrollment service custody, then reopen the same state
    // file. The governor is moved, never cloned or reconstructed.
    receiving_custody.lock().unwrap().take();
    let mut governor = authority.lock().unwrap().take().unwrap();
    drop(owner);
    let path = directory.path().join("state.json");
    let mut tree = crate::shell::persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    let owner =
        crate::shell::native_workspace_owner::open(Arc::new(parking_lot::RwLock::new(tree)))
            .unwrap();
    owner
        .with_current(room, |view| {
            assert_eq!(view.peer().replay_floors(), floors);
            let message: NeighbourMsg = postcard::from_bytes(&original).unwrap();
            assert!(
                view.peer()
                    .lookup_retained_local_document(
                        &message,
                        identity.device_cert.device_public_key
                    )
                    .unwrap()
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        owner
            .outgoing_status(room, SystemTime::now())
            .unwrap()
            .pending,
        2
    );
    assert_eq!(
        owner
            .with_outgoing(room, SystemTime::now(), |_, outgoing| Ok(outgoing
                .entries()
                .count()))
            .unwrap(),
        0,
        "restored projection must wait for genuine current governor freshness"
    );

    let (mailbox, mut requests) = tilo_relay::peer::sync_refresh::Mailbox::bounded();
    relay.install_sync_refresh(topic, mailbox).await.unwrap();
    let (commands, actor) = crate::network::actor::Actor::spawn_reporting_addrs(
        endpoint.clone(),
        iroh::address_lookup::memory::MemoryLookup::new(),
        Some(identity.clone()),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (events, mut received) = tokio::sync::mpsc::channel(64);
    commands
        .send(crate::network::actor::NetCmd::JoinWorkspace {
            ws: topic,
            on_event: events,
        })
        .await
        .unwrap();
    commands
        .send(crate::network::actor::NetCmd::NativeConnect(
            crate::network::actor::native_ingress::Connect::new(owner.clone(), room).unwrap(),
        ))
        .await
        .unwrap();
    // Pause owner admission of the prior replay while proving the new startup
    // request independently. Keep those exact inputs bounded and alive; no fake
    // expired checkpoint or clock override is introduced into the product.
    let mut prior_replay = Vec::new();
    tilo_test_env::within("the actor to report HostReady", async {
        loop {
            match received.recv().await.expect("current actor") {
                crate::network::actor::NetEvent::HostReady { .. } => break,
                crate::network::actor::NetEvent::NativeInput(input) => {
                    assert!(prior_replay.len() < 32);
                    prior_replay.push(input);
                }
                _ => {}
            }
        }
    })
    .await;
    let request = tilo_test_env::within(
        "Ready to request the original governor refresh",
        requests.recv(),
    )
    .await
    .unwrap();
    refresh_on_request(&mut governor, request).await.unwrap();
    let fresh = publish_committed(&governor, publisher);
    admit_exact(&mut received, &commands, identity, &fresh).await;
    assert!(
        owner
            .with_outgoing(room, SystemTime::now(), |_, outgoing| Ok(outgoing
                .entries()
                .count()))
            .unwrap()
            > 0
    );
    // The original paused replay is no longer needed for the observed causal
    // proof; admission of fresh messages alone enabled the genuine projection.
    drop(prior_replay);

    tilo_test_env::within(
        "the live member session to resend its retained projection",
        async {
            loop {
                if let Some(tilo_relay::Inbound::Delta { author, data }) = publisher.recv().await {
                    if author != identity.device_cert.device_public_key {
                        continue;
                    }
                    let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                    governor
                        .receive(
                            tilo_peer_state::TransportOrigin::RelayDelta(author),
                            message,
                        )
                        .unwrap();
                    let doc =
                        AutoCommit::load(&governor.peer().document("so.tilo.chat", "").unwrap())
                            .unwrap();
                    if doc.get(ROOT, "real-clock-offline-edit").unwrap().is_some() {
                        for head in &expected_heads {
                            assert!(doc.get_change_by_hash(head).is_some());
                        }
                        break;
                    }
                }
            }
        },
    )
    .await;
    assert!(
        owner
            .outgoing_status(room, SystemTime::now())
            .unwrap()
            .pending
            > 0,
        "transport enqueue/receive is not a durable semantic ACK"
    );
    // Semantic retirement is deliberately left to the next real requested
    // checkpoint. This regression proves resend and that enqueue is not ACK.
    commands
        .send(crate::network::actor::NetCmd::LeaveWorkspace { ws: topic })
        .await
        .unwrap();
    drop(commands);
    actor.abort();
    let _ = actor.await;
}

/// The governor's answer to the relay's authenticated checkpoint request: a
/// member that may still read the room gets a freshly signed checkpoint.
async fn refresh_on_request(
    governor: &mut TestGovernor,
    request: tilo_relay::peer::sync_refresh::Request,
) -> Result<(), String> {
    request
        .execute(|workspace, certificate| {
            if workspace != governor.workspace_id() {
                return Err("checkpoint refresh owner is unavailable".into());
            }
            let peer = governor.peer();
            peer.permissions(certificate.device_public_key, SystemTime::now())?;
            if !peer.certificates().contains(certificate) {
                return Err("checkpoint refresh certificate is no longer current".into());
            }
            governor.refresh_checkpoint()
        })
        .await
}

fn publish_committed(
    owner: &TestGovernor,
    publisher: &tilo_relay::RelayCoeditSession,
) -> Vec<Vec<u8>> {
    let mut sent = Vec::new();
    for (bytes, kind) in owner.outgoing() {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let (channel, _) = tilo_peer_state::transport::retention(
            &message,
            kind == tilo_peer_state::transport::Retention::Checkpoint,
        )
        .unwrap();
        match kind {
            tilo_peer_state::transport::Retention::Checkpoint => {
                publisher.publish_checkpoint(channel, bytes.to_vec())
            }
            tilo_peer_state::transport::Retention::Delta => {
                publisher.publish_retained_delta(channel, bytes.to_vec())
            }
        }
        .unwrap();
        sent.push(bytes.to_vec());
    }
    sent
}
/// Admit exactly `messages` the way the shell dispatcher does: each admission
/// is followed by `NativePublish`, because governor admission can enable a
/// retained successor that the live session should send without waiting for
/// the next heartbeat.
async fn admit_exact(
    received: &mut tokio::sync::mpsc::Receiver<crate::network::actor::NetEvent>,
    commands: &tokio::sync::mpsc::Sender<crate::network::actor::NetCmd>,
    identity: &HostIdentity,
    messages: &[Vec<u8>],
) {
    let mut seen = std::collections::BTreeSet::new();
    tilo_test_env::within("every expected governor input", async {
        while seen.len() < messages.len() {
            if let Some(crate::network::actor::NetEvent::NativeInput(input)) = received.recv().await
            {
                let bytes = postcard::to_allocvec(input.message()).unwrap();
                if let Some(index) = messages.iter().position(|expected| *expected == bytes) {
                    input
                        .admit(identity)
                        .expect("real refreshed governor input must cross A barrier");
                    commands
                        .send(crate::network::actor::NetCmd::NativePublish {
                            ws: crate::shell::network::room_topic_id(input.room()),
                        })
                        .await
                        .unwrap();
                    seen.insert(index);
                }
            }
        }
    })
    .await;
}
