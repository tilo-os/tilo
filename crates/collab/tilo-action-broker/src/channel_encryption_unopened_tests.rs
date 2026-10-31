/// A room whose governance this device learns of later, as the workspace
/// tree does when a room's authority arrives: it announces the change.
#[derive(Clone, Default)]
struct LateGovernor {
    governor: Arc<Mutex<Option<Ed25519PublicKey>>>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl ChannelDirectory for LateGovernor {
    fn governor_of(&self, _: RoomId) -> Option<Ed25519PublicKey> {
        *self.governor.lock().unwrap()
    }
    fn allowed_devices(
        &self,
        _: RoomId,
        _: ChannelId,
        _: Option<&ChannelRoute>,
        _: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        Some(BTreeSet::new())
    }
    fn policy_epoch(&self) -> Option<u64> {
        Some(self.epoch.load(Ordering::SeqCst))
    }
}

fn late_governor_service(dir: &Path, directory: &LateGovernor) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: dir.join("channels"),
            vault: VaultKey::new([21; 32]),
            device_key: key(21),
        },
        Box::new(directory.clone()),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap()
}

/// A route asked about before its room's governance is known is told
/// when it is learned, with no timer running meanwhile: the endpoint need
/// not ask on one to find out.
#[test]
fn a_route_hears_when_its_rooms_governance_is_learned() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateGovernor::default();
    let mut service = late_governor_service(dir.path(), &directory);
    let (room, channel) = (RoomId::new(), ChannelId::new());
    let transport = Endpoint::join(&Arc::new(Room::default()), room, 0);
    let mut now = Instant::now();
    assert!(matches!(
        service.access(room, channel, now),
        ChannelAccess::Unavailable
    ));
    assert!(
        std::mem::take(&mut service.queued),
        "a newly unopened channel wakes a parked pump to arm its watch"
    );
    service.pump(&transport, now, 1_000);
    assert_eq!(service.channel_change(channel), 0);
    assert_eq!(
        service.next_pump_deadline(now),
        None,
        "nothing is due while the room's governance is unknown"
    );
    assert!(
        service.waits_for_policy_change(),
        "the pump waits for an announcement instead"
    );

    // Announcements that leave the room's governance unknown tell no
    // route.
    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_005);
    assert_eq!(service.channel_change(channel), 0);

    *directory.governor.lock().unwrap() = Some(public(22));
    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_010);
    let told = service.channel_change(channel);
    assert_ne!(
        told, 0,
        "the route is told its room's governance is known now"
    );
    assert!(matches!(
        service.access(room, channel, now),
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty());

    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_015);
    assert_eq!(
        service.channel_change(channel),
        told,
        "an opened channel is not told again"
    );
}

/// A channel whose state could not be opened is retried by the pump on
/// its own backoff, each retry telling the route to ask again.
#[test]
fn a_failed_open_is_retried_on_the_pumps_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateGovernor::default();
    *directory.governor.lock().unwrap() = Some(public(22));
    let mut service = late_governor_service(dir.path(), &directory);
    let (room, channel) = (RoomId::new(), ChannelId::new());
    let transport = Endpoint::join(&Arc::new(Room::default()), room, 0);
    // The room's state directory cannot be created: a file is in its way.
    let blocker = dir.path().join("channels").join(room.to_string());
    std::fs::write(&blocker, b"").unwrap();
    let now = Instant::now();
    assert!(matches!(
        service.access(room, channel, now),
        ChannelAccess::Unavailable
    ));
    service.pump(&transport, now, 1_000);
    let due = service
        .next_pump_deadline(now)
        .expect("the failed open is due again");
    assert_eq!(due, now.checked_add(REQUEST_FIRST).unwrap());
    assert_eq!(service.channel_change(channel), 0);
    service.pump(&transport, due, 1_002);
    let told = service.channel_change(channel);
    assert_ne!(told, 0, "the retry tells the route to ask again");
    assert_eq!(
        service.next_pump_deadline(due),
        Some(due.checked_add(REQUEST_FIRST * 2).unwrap()),
        "and backs off"
    );

    std::fs::remove_file(&blocker).unwrap();
    assert!(matches!(
        service.access(room, channel, due),
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty(), "an open that succeeds settles");
}
