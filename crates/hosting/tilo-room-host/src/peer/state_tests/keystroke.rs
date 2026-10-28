//! Count complete writes at the production writer boundary, not QUIC timing.
use super::*;
use collab_types::{PeerPresence, PeerPresencePayload};
use neighbour_wire::NeighbourMsg;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

#[derive(Default)]
struct CountedWire {
    writes: usize,
    bytes: Vec<u8>,
}
impl AsyncWrite for CountedWire {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.writes = self.writes.checked_add(1).unwrap();
        self.bytes.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn presence(payload: PeerPresencePayload) -> NeighbourMsg {
    NeighbourMsg::AppPresenceV2 {
        peer: 1,
        app_id: "so.tilo.word-processor".into(),
        cluster_id: "writer-room".into(),
        presence: PeerPresence {
            key: br#"{"widget":{"path":[{"field":"body"}]}}"#.to_vec(),
            payload,
        },
        colour: None,
    }
}

#[tokio::test(start_paused = true)]
async fn keystroke_presence_and_position_share_the_production_delta_write() {
    let mut fixture = Fixture::new();
    let mailbox = Arc::new(crate::presence_mailbox::PresenceMailbox::default());
    fixture
        .workspace
        .lock()
        .await
        .route_presence(fixture.recipient.device_public_key, mailbox.clone());
    let resumes = std::sync::Mutex::new(HashMap::from([(
        7,
        ResumeLog {
            store: fixture.store.clone(),
            issuer: crate::position::PositionIssuer::new(&fixture.source_key.to_bytes()),
            device: fixture.recipient.device_public_key,
            generation: fixture.store.generation().unwrap(),
            resume: crate::position::Resume {
                log: fixture
                    .store
                    .start_resume(&fixture.recipient.device_public_key)
                    .unwrap(),
                len: 0,
            },
        },
    )]));
    let agents = AgentRegistry::default();
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
        metrics: &fixture.metrics,
        trust_policy: &fixture.source.policy,
        disconnect: &fixture.recipient_disconnect,
        presence: Some(&mailbox),
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
    let source_session = SessionCtx {
        outbound: &fixture.source.tx,
        disconnect: &fixture.source.disconnect,
        ..session
    };
    let mut coding = SessionCoding::default();
    let mut wire = CountedWire::default();
    let mut counts = Vec::new();
    for pos in 1..=200u64 {
        let presence_precedes_write = pos <= 100;
        let before = wire.writes;
        if !presence_precedes_write {
            // Persistence and the writer may finish before ingress routes
            // the following typing and caret: that valid schedule costs two.
            fixture.enqueue(2).await;
            fixture.drained().await;
            let delivery = fixture.recipient_rx.recv().await.unwrap();
            write_state_delivery(
                delivery,
                &fixture.recipient,
                &session,
                &mut coding,
                &mailbox,
                &mut wire,
            )
            .await
            .unwrap();
        }
        // Reproduce the ingress state while the author's delta is admitted
        // but not yet published. No transport or persistence latency decides
        // when the recipient writer gets its turn.
        if presence_precedes_write {
            fixture.workspace.lock().await.note_delta(
                fixture.source.peer.device_public_key,
                tokio::time::Instant::now(),
            );
        }
        let messages = [
            presence(PeerPresencePayload::Typing {
                start: pos - 1,
                end: pos,
            }),
            presence(PeerPresencePayload::Caret { pos }),
        ];
        for message in &messages {
            let payload = crate::AttributedPresence {
                author: fixture.source.peer.device_public_key,
                body: postcard::to_allocvec(message).unwrap(),
            }
            .encode()
            .unwrap();
            handle_presence(
                &fixture.source.peer,
                &source_session,
                &[7],
                Envelope::Presence {
                    workspace_id: 7,
                    payload,
                },
            )
            .await
            .unwrap();
        }
        // Past the ordinary batching window, but before the delta hold:
        // removing the hold must send a separate presence write here.
        tokio::time::advance(
            crate::presence_mailbox::BATCH_WINDOW + std::time::Duration::from_millis(1),
        )
        .await;
        if presence_precedes_write {
            assert!(
                mailbox.take(tokio::time::Instant::now(), false).is_empty(),
                "keystroke presence became due before its admitted delta"
            );
            fixture.enqueue(2).await;
            fixture.drained().await;
            let delivery = fixture.recipient_rx.recv().await.unwrap();
            write_state_delivery(
                delivery,
                &fixture.recipient,
                &session,
                &mut coding,
                &mailbox,
                &mut wire,
            )
            .await
            .unwrap();
        }
        write_presence(
            &mailbox,
            &mut coding.presence,
            &fixture.recipient,
            &session,
            &mut wire,
        )
        .await
        .unwrap();
        counts.push(wire.writes - before);
        let expected = if presence_precedes_write { 1 } else { 2 };
        assert_eq!(
            counts.last(),
            Some(&expected),
            "write count must follow the controlled production schedule"
        );
        tokio::time::advance(crate::presence_mailbox::DELTA_HOLD).await;
    }
    let mut reader =
        tilo_delivery_protocol::transport_v2::SessionReader::new(wire.bytes.as_slice());
    // Decode with the actual session reader, retaining coding state across
    // keystrokes rather than comparing encoder internals.
    for pos in 1..=200u64 {
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::State(
                fixture.envelope(2)
            ))
        );
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::State(
                Envelope::Position { .. }
            ))
        ));
        for expected in [
            presence(PeerPresencePayload::Typing {
                start: pos - 1,
                end: pos,
            }),
            presence(PeerPresencePayload::Caret { pos }),
        ] {
            let Some(tilo_delivery_protocol::transport_v2::EnvelopeV2::State(Envelope::Presence {
                payload,
                ..
            })) = reader.read_next().await.unwrap()
            else {
                panic!("missing keystroke presence")
            };
            let attributed = crate::AttributedPresence::decode(&payload).unwrap();
            assert_eq!(attributed.author, fixture.source.peer.device_public_key);
            assert_eq!(
                postcard::from_bytes::<NeighbourMsg>(&attributed.body).unwrap(),
                expected
            );
        }
    }
    assert!(reader.read_next().await.unwrap().is_none());
    println!(
        "KEYSTROKE_WRITES queued_presence={:?} late_presence={:?}",
        &counts[..100],
        &counts[100..]
    );
}
