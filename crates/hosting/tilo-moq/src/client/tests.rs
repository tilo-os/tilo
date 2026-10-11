use super::*;

fn connection() -> Connection {
    Connection {
        scope: ConnectionScope {
            endpoint: "https://example.test/moq/voice".into(),
            fingerprints: vec![],
            owner_epoch: [1; 32],
            carrier_generation: 1,
            publication: Some(format!("{}/{}", "01".repeat(32), "02".repeat(32))),
            subscriptions: vec![],
        },
        authorization: vec![1],
    }
}
#[test]
fn inert_connection_validates_raw_endpoint_and_closed_voice_scope_only() {
    assert!(validate(&connection()).is_ok()); // Structural validity confers no readiness.
    for endpoint in [
        "http://example.test/moq/voice",
        "wss://example.test/moq/voice",
        "moqt://example.test/moq/voice",
        "https://example.test:443/moq/voice",
        "https://EXAMPLE.test/moq/voice",
        "https://example.test/moq/../moq/voice",
        "https://example.test/moq/%76oice",
        "https://user@example.test/moq/voice",
        "https://example.test/moq/voice?q=1",
        "https://example.test/moq/voice#x",
    ] {
        let mut connection = connection();
        connection.scope.endpoint = endpoint.into();
        assert!(validate(&connection).is_err(), "{endpoint}");
    }
    for name in ["audio", "call/publisher", "", "01/02"] {
        let mut connection = connection();
        connection.scope.publication = Some(name.into());
        assert!(validate(&connection).is_err());
    }
    let mut altered = connection();
    altered
        .scope
        .subscriptions
        .push(altered.scope.publication.clone().unwrap());
    assert!(validate(&altered).is_err());
    altered.scope.subscriptions = vec![format!("{}/{}", "03".repeat(32), "04".repeat(32))];
    assert!(validate(&altered).is_err());
    altered = connection();
    altered.authorization = vec![1; crate::relay::MAX_AUTHORIZATION_BYTES + 1];
    assert!(validate(&altered).is_err());
    altered = connection();
    altered.scope.owner_epoch = [0; 32];
    assert!(validate(&altered).is_err());
}

struct DeniedOwner;
impl ClientAuthority for DeniedOwner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        false
    }
}
#[tokio::test]
async fn inert_proof_input_cannot_mint_authority_or_readiness() {
    assert!(matches!(
        Client::start(connection(), Arc::new(DeniedOwner)),
        Err(ClientError::Denied)
    ));
}

fn timestamp(frame: u64) -> u64 {
    frame.saturating_mul(u64::from(voice::FRAME_SAMPLES))
}
fn object(sequence: u64, frame: u64) -> VoiceObject {
    VoiceObject {
        sequence,
        timestamp: timestamp(frame),
        duration: voice::FRAME_SAMPLES,
        payload: vec![1],
    }
}
#[test]
fn frames_missing_between_deliveries_count_as_dropped() {
    let at = |sequence, frame| Delivered {
        sequence,
        timestamp: timestamp(frame),
    };
    for grouped in [false, true] {
        assert_eq!(at(7, 7).skipped(&object(8, 8), grouped), 0);
        assert_eq!(at(7, 7).skipped(&object(18, 18), grouped), 10);
        // An abandoned partial group: sequence numbers jump to the next group,
        // media time does not.
        assert_eq!(at(12, 12).skipped(&object(15, 13), grouped), 0);
        assert_eq!(at(12, 12).skipped(&object(20, 18), grouped), 5);
    }
}

#[test]
fn a_dtx_pause_is_silence_not_loss() {
    let at = |sequence, frame| Delivered {
        sequence,
        timestamp: timestamp(frame),
    };
    // A 2 s DTX pause (100 frames of media time) from a datagram publisher:
    // its sequence numbers stay contiguous, so nothing was lost.
    for (sequence, frame) in [(41_u64, 100_u64), (42, 120), (43, 140)] {
        let prior = at(sequence.saturating_sub(1), frame.saturating_sub(20));
        assert_eq!(prior.skipped(&object(sequence, frame), false), 0);
    }
    assert_eq!(at(40, 0).skipped(&object(41, 100), false), 0);
    // A grouped publisher closes the group of its last speech frame when that
    // frame expires and sends its next comfort-noise update in a new group.
    for last in 10..15 {
        assert_eq!(at(last, 0).skipped(&object(15, 100), true), 0, "{last}");
    }
    // A whole group missing in between is still loss, at most the media gap.
    assert_eq!(at(12, 0).skipped(&object(20, 100), true), 5);
    assert_eq!(at(12, 0).skipped(&object(20, 3), true), 2);
}

struct CurrentOwner;
impl ClientAuthority for CurrentOwner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}
const NAME: &str = "remote";
/// A live client's shared state without a carrier.
fn live_shared() -> Shared {
    let (outgoing, outgoing_task) =
        carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, SEND_AGE));
    let (incoming, incoming_task) =
        carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, RECEIVE_AGE));
    Shared {
        scope: connection().scope,
        authority: Arc::new(CurrentOwner),
        outgoing,
        incoming,
        _origins: [outgoing_task, incoming_task],
        state: Mutex::new(State {
            status: Status::Live,
            muted: true,
            sequence: 0,
            session: None,
            broadcast: None,
            track: None,
            groups: VecDeque::new(),
            received: BTreeMap::new(),
            counters: Counters::default(),
        }),
    }
}

#[tokio::test]
async fn a_two_second_dtx_pause_counts_no_dropped_frames_on_either_carrier_form() {
    // The receive path of a real client, without a carrier: deliver() is
    // what both the datagram and the group receive loops call.
    let shared = live_shared();
    let last = Mutex::new(None);
    // (sequence, frame, grouped): a datagram publisher's 7 speech frames, 2 s
    // of DTX and 3 more; then a grouped publisher whose 3-frame spurt ended
    // mid-group, so that group was closed and its next frame, 2 s later,
    // opens the following group.
    let mut sent = Vec::new();
    sent.extend((0..7).map(|n| (n, n, false)));
    sent.extend((7..10).map(|n: u64| (n, n.saturating_add(100), false)));
    sent.extend((10..13).map(|n: u64| (n, n.saturating_add(200), true)));
    sent.push((15, 313, true));
    for (sequence, frame, grouped) in sent {
        assert!(deliver(
            &shared,
            NAME,
            sequence / voice::FRAMES_PER_GROUP,
            grouped,
            object(sequence, frame),
            &last
        ));
    }
    let counters = shared.state.lock().unwrap().counters;
    assert_eq!(counters.received, 14);
    assert_eq!(counters.late_dropped, 0, "DTX pauses are not loss");
    assert_eq!(counters.duplicate_dropped, 0);
}

fn write_group_frame(group: &mut moq_net::group::Producer, frame: u64) {
    group
        .write_frame(
            moq_net::Timestamp::from_scale(timestamp(frame), 48_000).unwrap(),
            object(frame, frame).encode().unwrap(),
        )
        .unwrap();
}

async fn poll_group_receiver(
    mut receiving: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
) {
    let pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(receiving.as_mut(), cx).is_pending())
    })
    .await;
    assert!(
        pending,
        "the active production receiver must retain its track"
    );
}

fn drain_group_sequences(shared: &Shared, received: &mut Vec<u64>) {
    let mut state = shared.state.lock().unwrap();
    if let Some(queue) = state.received.get_mut(NAME) {
        received.extend(queue.drain(..).map(|frame| frame.object.sequence));
    }
}

#[tokio::test]
async fn a_new_group_keeps_the_previous_unexpired_tail() {
    let shared = live_shared();
    let broadcast = shared.incoming.create_broadcast(NAME).unwrap();
    let publisher = broadcast.create_track(VOICE_TRACK, None).unwrap();
    let subscriber = publisher
        .consume()
        .subscribe(moq_net::track::Subscription::default().with_max_age(RECEIVE_AGE))
        .await
        .unwrap();
    let last = Mutex::new(None);
    let receiving = receive_groups(&shared, NAME, subscriber, &last);
    tokio::pin!(receiving);
    let mut received = Vec::new();
    let mut next_group = 0_u64;
    let mut checkpoints = Vec::new();
    while next_group < 50 {
        let first = next_group.saturating_mul(voice::FRAMES_PER_GROUP);
        let mut group = publisher
            .create_group(moq_net::group::Info::from(next_group))
            .unwrap();
        if first == 65 {
            // QUIC group streams can complete in a different order. Make the
            // newer group readable while frame 69 is still in flight, without
            // expiring the original group or changing its authority.
            for frame in first..69 {
                write_group_frame(&mut group, frame);
            }
            poll_group_receiver(receiving.as_mut()).await;
            drain_group_sequences(&shared, &mut received);
            assert_eq!(received, (0..69).collect::<Vec<_>>());
            let mut newer = publisher
                .create_group(moq_net::group::Info::from(next_group.saturating_add(1)))
                .unwrap();
            for frame in 70..75 {
                write_group_frame(&mut newer, frame);
            }
            newer.finish().unwrap();
            poll_group_receiver(receiving.as_mut()).await;
            drain_group_sequences(&shared, &mut received);
            checkpoints.push(received.clone());
            write_group_frame(&mut group, 69);
            group.finish().unwrap();
            poll_group_receiver(receiving.as_mut()).await;
            drain_group_sequences(&shared, &mut received);
            checkpoints.push(received.clone());
            next_group = next_group.saturating_add(2);
        } else {
            for frame in first..first.saturating_add(voice::FRAMES_PER_GROUP) {
                write_group_frame(&mut group, frame);
            }
            group.finish().unwrap();
            poll_group_receiver(receiving.as_mut()).await;
            drain_group_sequences(&shared, &mut received);
            next_group = next_group.saturating_add(1);
        }
    }
    publisher.finish().unwrap();
    tilo_test_env::within("all 250 original grouped frames", receiving).await;
    drain_group_sequences(&shared, &mut received);
    let counters = shared.state.lock().unwrap().counters;
    eprintln!(
        "all250frames: {counters:?}; missing={:?}",
        (0..250)
            .filter(|frame| !received.contains(frame))
            .collect::<Vec<_>>()
    );
    assert_eq!(counters.received, 250);
    assert_eq!(received, (0..250).collect::<Vec<_>>());
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[0],
        (0..69).collect::<Vec<_>>(),
        "a newer group must not retire the original unexpired frame 69"
    );
    assert_eq!(checkpoints[1], (0..75).collect::<Vec<_>>());
    assert_eq!(counters.late_dropped, 0);
    assert_eq!(counters.duplicate_dropped, 0);
    assert_eq!(counters.capacity_dropped, 0);
}
