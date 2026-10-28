# A Lounge the relay kept in the clear

`state.tar.gz.b64` holds, as base64 text, a relay's state directory as
tilo-relay kept it at d56f3c7bd9, before the records of a room whose channels the relay
stewards were sealed from their first write. The room is "Lounge",
governed and stewarded by the fixture governor (`rooms::tests::governor_with`
with host capabilities). In it:

- the host wrote three records (`welcome`, `chat`, `notes` of
  `so.tilo.notes`) in the clear;
- two member devices (seeds 3 and 4) wrote each of them six times in the
  clear, through their connections, so the relay retains their writes;
- the members then opened the channels of `welcome` and `chat` and wrote
  each four times sealed; `notes` never got a channel.

`reseal::tests` opens it with the current relay. It was produced by the
test below, added to `crates/hosting/tilo-relay/src/rooms/sealed_records/tests.rs`
at d56f3c7bd9 and run with
`cargo test -p tilo-relay --features agents --lib -- --ignored legacy_lounge_fixture --nocapture`;
the hex it prints between the markers is the gzipped tar this file
encodes (`xxd -r -p | base64 -w 76`).

```rust

/// A device's connection to the relay, subscribed to `workspace_id`.
async fn connected(
    relay: &crate::daemon::Relay,
    (key, cert): (&SigningKey, DeviceCert),
    workspace_id: u64,
) -> crate::RelayClient {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let client = crate::RelayClient::connect_to_pinned_host(
        endpoint,
        relay.node_addr(),
        cert,
        key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(crate::Envelope::Subscribe { workspace_id })
        .unwrap();
    tilo_test_env::within("the subscription", async {
        while !matches!(client.recv().await, Some(crate::Envelope::Ready { .. })) {}
    })
    .await;
    client
}

fn send(client: &crate::RelayClient, key: &SigningKey, workspace_id: u64, message: &NeighbourMsg) {
    let (channel, _) = neighbour_wire::retention(message, false).unwrap();
    client
        .send(crate::Envelope::Delta {
            workspace_id,
            author_device_public_key: key.verifying_key().to_bytes(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: crate::CoeditPayload::RetainedDelta {
                channel,
                body: postcard::to_stdvec(message).unwrap(),
            }
            .encode()
            .unwrap(),
            signature: [0; 64],
        })
        .unwrap();
}

/// Wait until the room reads `key` of `record` as `value`.
async fn reads(rooms: &Rooms, room: &str, record: &str, key: &str, value: &str) {
    tilo_test_env::within("the room takes the write in", async {
        loop {
            let doc = rooms
                .record_document(room, APP, record)
                .await
                .unwrap()
                .map(|bytes| automerge::AutoCommit::load(&bytes).unwrap());
            let read = doc.and_then(|doc| {
                doc.get(ROOT, key)
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok())
            });
            if read.as_deref() == Some(value) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// A member's write of `record` in the clear, as a page wrote it before
/// the record's channel was opened.
async fn clear_write(
    rooms: &Rooms,
    room: &str,
    key: &SigningKey,
    record: &str,
    (field, value): (&str, &str),
) -> NeighbourMsg {
    let state = rooms.room(room).await.unwrap();
    let mut scratch = state.lock().await.peer().clone();
    let mut doc = automerge::AutoCommit::load(&scratch.document(APP, record).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit("fixture", field);
    scratch
        .publish_incremental(APP, record, doc.save_after(&heads), key, SystemTime::now())
        .unwrap()
}

/// Produces `crates/hosting/tilo-relay/tests/fixtures/legacy-lounge.tar.gz`
/// with the relay as it was before records of a stewarded room were sealed
/// from their first write: a Lounge whose records the host and two members
/// wrote in the clear, two of them later written sealed by members holding
/// their channels' keys, one never given a channel. Prints the state
/// directory, gzipped tar, hex, between markers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "produces a fixture"]
async fn legacy_lounge_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Lounge").await.unwrap();
    let (room, workspace_id) = (created.room, created.workspace_id);
    for record in ["welcome", "chat", "notes"] {
        rooms
            .author_room_record(&room, &cert, (APP, record), |doc| {
                doc.put(ROOT, "title", format!("{record} by the host"))
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
    }
    let mut members = Vec::new();
    for seed in [3, 4] {
        let (member_key, member_cert) = member_device(&rooms, &room, (&key, &cert), seed).await;
        {
            let state = rooms.room(&room).await.unwrap();
            let mut governed = state.lock().await;
            rooms.settle(&mut governed).await.unwrap();
        }
        let client = connected(&relay, (&member_key, member_cert), workspace_id).await;
        members.push((member_key, client));
    }
    for index in 0..6 {
        for record in ["welcome", "chat", "notes"] {
            let (member_key, client) = &members[index % 2];
            let value = format!("{record} line {index}");
            let message = clear_write(&rooms, &room, member_key, record, ("line", &value)).await;
            send(client, member_key, workspace_id, &message);
            reads(&rooms, &room, record, "line", &value).await;
        }
    }
    for (record, member) in [("welcome", 0), ("chat", 1)] {
        let (member_key, client) = &members[member];
        let sealing = SealingMember::join(&rooms, &room, (APP, record), member_key.clone()).await;
        // Past the three writes the member made in the clear.
        sealing.seq.store(3, std::sync::atomic::Ordering::Relaxed);
        for index in 0..4 {
            let value = format!("{record} sealed line {index}");
            let ((), message) = sealing.seal(&room, |doc| {
                doc.put(ROOT, "line", value.as_str()).unwrap();
                doc.commit_edit("fixture", "line");
            });
            send(client, member_key, workspace_id, &message);
            reads(&rooms, &room, record, "line", &value).await;
        }
    }
    tilo_test_env::within("the relay retains every write", async {
        loop {
            let welcome = retained_writes(&relay, workspace_id, "welcome").await;
            if welcome.iter().filter(|op| op.encryption.is_some()).count() >= 4 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    drop(members);
    drop(rooms);
    relay.shutdown().await;
    let archive = std::process::Command::new("tar")
        .args(["czf", "-", "-C"])
        .arg(dir.path())
        .arg(".")
        .output()
        .unwrap();
    assert!(archive.status.success());
    println!("ROOM {room} WORKSPACE {workspace_id}");
    println!("FIXTURE-BEGIN");
    for chunk in archive.stdout.chunks(4096) {
        println!("{}", hex::encode(chunk));
    }
    println!("FIXTURE-END");
}
```
