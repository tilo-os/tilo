//! Presence byte budgets on the actual writer, with a controlled ingress schedule.
use super::*;
use collab_types::{PeerPresence, PeerPresencePayload};
use neighbour_wire::NeighbourMsg;
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader, StateFraming};

const ROOM: &str = "5e11f00d-7110-4625-8000-000000000710";
const MOVES: u32 = 60;

fn pointer(step: u32) -> NeighbourMsg {
    NeighbourMsg::PresenceMoved {
        peer: 0x9e37_79b9_7f4a_7c15,
        user: "a".repeat(64),
        key: format!(r#"{{"pointer":{{"space":{{"tile":{{"room":"{ROOM}","tab":1}}}}}}}}"#),
        point: (f64::from(step).mul_add(3.0, 200.0), 300.0),
        ts: 1_800_000_000_000u64
            .checked_add(u64::from(step).checked_mul(10).unwrap())
            .unwrap(),
        colour: Some("teal".into()),
    }
}

fn caret(pos: u64) -> NeighbourMsg {
    NeighbourMsg::AppPresenceV2 {
        peer: 0x9e37_79b9_7f4a_7c15,
        app_id: "so.tilo.word-processor".into(),
        cluster_id: ROOM.into(),
        presence: PeerPresence {
            key: br#"{"widget":{"path":[{"field":"body"}]}}"#.to_vec(),
            payload: PeerPresencePayload::Caret { pos },
        },
        colour: Some("teal".into()),
    }
}

async fn cost(fixture: &Fixture, framing: StateFraming) -> (usize, u64, u64) {
    let mailbox = Arc::new(crate::presence_mailbox::PresenceMailbox::default());
    fixture
        .workspace
        .lock()
        .await
        .route_presence(fixture.recipient.device_public_key, mailbox.clone());
    let metrics = Arc::new(crate::metrics::Metrics::default());
    let agents = AgentRegistry::default();
    let resumes = std::sync::Mutex::default();
    let lag = Arc::default();
    let session = SessionCtx {
        workspaces: &fixture.registry,
        autobind: None,
        outbound: &fixture.recipient_tx,
        state_outbound: None,
        connection_live: None,
        own_user_public_key: &fixture.source.own_user,
        own_device_cert: &fixture.source.peer.cert,
        own_device_public_key: &fixture.source.peer.cert.device_public_key,
        app_serving: None,
        metrics: &metrics,
        trust_policy: &fixture.source.policy,
        disconnect: &fixture.recipient_disconnect,
        framing,
        presence: Some(&mailbox),
        signed_actions: false,
        private_tx: &fixture.private,
        revocations: &fixture.source.revocations,
        live_bridge: None,
        agents: &agents,
        positions: None,
        compactor: None,
        member_limits: None,
        resumes: &resumes,
        lag: &lag,
    };
    let source = SessionCtx {
        outbound: &fixture.source.tx,
        disconnect: &fixture.source.disconnect,
        ..session
    };
    let mut bytes = Vec::new();
    let mut wire =
        crate::metrics::EgressWrite::new(&mut bytes, &metrics, fixture.recipient.device_public_key);
    let mut slots = tilo_delivery_protocol::transport_v2::PresenceEncoder::default();
    // The older client's 100 Hz input, without transport or executor latency
    // changing which samples reach the relay's 20 Hz writer. Classification,
    // author stamping, mailbox routing and frame encoding are all production.
    let mut input: Vec<_> = (0..MOVES).map(pointer).collect();
    input.extend((30..=42).map(caret));
    let cleared = NeighbourMsg::PresenceCleared {
        peer: 0x9e37_79b9_7f4a_7c15,
        key: "tile".into(),
    };
    input.push(cleared.clone());
    for (index, message) in input.iter().enumerate() {
        let payload = crate::AttributedPresence {
            author: fixture.source.peer.device_public_key,
            body: postcard::to_allocvec(message).unwrap(),
        }
        .encode()
        .unwrap();
        handle_presence(
            &fixture.source.peer,
            &source,
            &[7],
            Envelope::Presence {
                workspace_id: 7,
                payload,
            },
        )
        .await
        .unwrap();
        if index < usize::try_from(MOVES).unwrap() {
            if mailbox
                .next_due()
                .is_some_and(|at| at <= tokio::time::Instant::now())
            {
                write_presence(
                    &mailbox,
                    &mut slots,
                    &fixture.recipient,
                    &session,
                    &mut wire,
                )
                .await
                .unwrap();
            }
            tokio::time::advance(std::time::Duration::from_millis(10)).await;
        }
    }
    // The unslotted clear is a real ordering barrier: it flushes the final
    // pointer and caret, not a test-only encoder call or settling sleep.
    mailbox.ready().await;
    write_presence(
        &mailbox,
        &mut slots,
        &fixture.recipient,
        &session,
        &mut wire,
    )
    .await
    .unwrap();
    assert!(mailbox.next_due().is_none());
    let counters = metrics.session_egress();
    assert_eq!(counters.len(), 1);
    let sent = counters[0].bytes_sent;
    let writes = counters[0].writes;
    drop(wire);
    assert_eq!(sent, u64::try_from(bytes.len()).unwrap());
    assert_eq!(metrics.snapshot().bytes_sent_live, sent);
    let mut reader = SessionReader::framed(bytes.as_slice(), framing);
    let mut seen = Vec::new();
    while let Some(frame) = reader.read_next().await.unwrap() {
        let EnvelopeV2::State(Envelope::Presence {
            workspace_id,
            payload,
        }) = frame
        else {
            panic!("unexpected presence writer output: {frame:?}")
        };
        assert_eq!(workspace_id, 7);
        let attributed = crate::AttributedPresence::decode(&payload).unwrap();
        assert_eq!(attributed.author, fixture.source.peer.device_public_key);
        seen.push(postcard::from_bytes::<NeighbourMsg>(&attributed.body).unwrap());
    }
    let mut expected: Vec<_> = (0..MOVES).step_by(5).map(pointer).collect();
    expected.extend([pointer(MOVES - 1), caret(42), cleared]);
    assert_eq!(
        seen, expected,
        "latest state and event ordering must survive coding"
    );
    assert_eq!(writes, 13, "twelve paced writes and the final event flush");
    (seen.len(), sent, writes)
}

#[tokio::test(start_paused = true)]
async fn slot_coded_presence_costs_under_seventy_bytes_and_a_third_of_legacy() {
    let fixture = Fixture::new();
    let (old_count, old_bytes, old_writes) = cost(&fixture, StateFraming::Compact).await;
    let (new_count, new_bytes, new_writes) = cost(&fixture, StateFraming::Presence).await;
    assert_eq!(old_count, new_count);
    assert_eq!(old_count, 15);
    let count = u64::try_from(new_count).unwrap();
    assert!(
        new_bytes <= 70 * count,
        "slot coding exceeded 70 bytes per presence: {new_bytes}/{count}"
    );
    assert!(
        old_bytes > 3 * new_bytes,
        "slot coding must cost under a third: old={old_bytes}, new={new_bytes}"
    );
    println!(
        "PRESENCE_COST presence={count} relay5_bytes={old_bytes} relay6_bytes={new_bytes} relay5_writes={old_writes} relay6_writes={new_writes}"
    );
}
