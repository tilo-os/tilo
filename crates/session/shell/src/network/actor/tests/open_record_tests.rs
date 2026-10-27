use super::*;

/// An earlier part of a chained record reaches a device that joins later
/// only once it asks: the room's host replays it the chain's head, and
/// `OpenRecord` (what a local app reading back asks for) has the host
/// send the earlier part, as the room's other documents arrive.
#[allow(
    clippy::too_many_lines,
    reason = "a host, a writer with two parts and a reader that joins later and reads back, in one scenario"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_device_opens_an_earlier_part_of_a_chain_at_the_host() {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
    let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
    let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
        &root,
        a_key.verifying_key().to_bytes(),
        b_key.verifying_key().to_bytes(),
    )
    .await;
    let identity = |key, cert| HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    };
    let app = "so.tilo.chat";
    let part = |index| neighbour_wire::record_part("derived:messages@room", index);
    let document = |record: &str, text: &str| {
        let mut op = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(text.as_bytes().to_vec())
                .encode()
                .unwrap(),
            collab_types::document_artifact_id(app, "room", record),
            collab_types::Ed25519PublicKey::from_bytes(a_key.verifying_key().to_bytes()),
            collab_types::document_attestation_id(app, "room", record),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut op, &a_key).unwrap();
        op
    };
    let first = document(&part(0), "the first messages");
    let second = document(&part(1), "the messages after the first part filled");

    let (a, lookup_a) = identified_endpoint(&a_key).await;
    let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
        a,
        lookup_a,
        Some(identity(a_key.clone(), a_cert)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (events_a, mut rx_a) = mpsc::channel(16);
    cmd_a
        .send(NetCmd::JoinWorkspace {
            ws: 42,
            on_event: events_a,
        })
        .await
        .unwrap();
    cmd_a
        .send(NetCmd::ConnectHost {
            addr: relay.node_addr(),
            expectation: None,
        })
        .await
        .unwrap();
    for (record, op) in [(part(0), &first), (part(1), &second)] {
        cmd_a
            .send(NetCmd::PublishToHosts {
                ws: 42,
                msg: NeighbourMsg::AppDocument {
                    peer: 7,
                    app_id: app.into(),
                    cluster_id: "room".into(),
                    record_id: record,
                    envelope: Box::new(op.clone()),
                },
            })
            .await
            .unwrap();
    }
    tilo_test_env::within("A's host session is ready", async {
        while !matches!(rx_a.recv().await, Some(NetEvent::HostReady { .. })) {}
    })
    .await;
    tilo_test_env::within("the host retains both parts", async {
        while relay
            .room_records()
            .retained(42)
            .await
            .unwrap_or_default()
            .len()
            < 2
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    let (b, lookup_b) = identified_endpoint(&b_key).await;
    let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
        b,
        lookup_b,
        Some(identity(b_key, b_cert)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (events_b, mut rx_b) = mpsc::channel(64);
    cmd_b
        .send(NetCmd::JoinWorkspace {
            ws: 42,
            on_event: events_b,
        })
        .await
        .unwrap();
    cmd_b
        .send(NetCmd::ConnectHost {
            addr: relay.node_addr(),
            expectation: None,
        })
        .await
        .unwrap();
    let mut replayed = Vec::new();
    tilo_test_env::within("B is replayed the room", async {
        loop {
            match rx_b.recv().await {
                Some(NetEvent::HostReady { .. }) => break,
                Some(NetEvent::Msg {
                    msg: NeighbourMsg::AppDocument { envelope, .. },
                    ..
                }) => replayed.push(*envelope),
                _ => {}
            }
        }
    })
    .await;
    assert!(replayed.contains(&second), "the head is replayed");
    assert!(
        !replayed.contains(&first),
        "an earlier part waits until it is asked for"
    );

    cmd_b
        .send(NetCmd::OpenRecord {
            ws: 42,
            channel: neighbour_wire::app_document_channel(app, &part(0)),
        })
        .await
        .unwrap();
    tilo_test_env::within("B receives the earlier part it asked for", async {
        loop {
            if let Some(NetEvent::Msg {
                msg:
                    NeighbourMsg::AppDocument {
                        envelope,
                        record_id,
                        ..
                    },
                ..
            }) = rx_b.recv().await
                && *envelope == first
            {
                assert_eq!(record_id, part(0));
                break;
            }
        }
    })
    .await;
}
