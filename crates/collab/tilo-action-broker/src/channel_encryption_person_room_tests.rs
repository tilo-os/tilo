/// A room no one governs (the person room) whose directory names its
/// channels' readers: this device founds the channel's group and is
/// answered its keys, a second device of the same identity is admitted,
/// and a device outside the identity is not.
#[test]
fn an_ungoverned_room_whose_readers_are_named_opens_and_admits_only_them() {
    /// A directory for a room no one governs: its channels' readers are
    /// the devices it names, and it knows no governor.
    struct Ungoverned(Policy);
    impl ChannelDirectory for Ungoverned {
        fn governor_of(&self, _: RoomId) -> Option<Ed25519PublicKey> {
            None
        }
        fn allowed_devices(
            &self,
            _: RoomId,
            channel: ChannelId,
            route: Option<&ChannelRoute>,
            now: u64,
        ) -> Option<BTreeSet<Ed25519PublicKey>> {
            if route.is_none() && self.0.unnamed.lock().unwrap().contains(&channel) {
                return None;
            }
            let mut allowed = self.0.allowed.lock().unwrap().clone();
            allowed.extend(
                self.0
                    .until
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, until)| now < **until)
                    .map(|(device, _)| *device),
            );
            Some(allowed)
        }
    }

    let room = Arc::new(Room::default());
    let room_id = RoomId::new();
    let channel = ChannelId::new();
    // The identity's two devices, not the outsider's.
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(1));
    policy.allowed.lock().unwrap().insert(public(2));
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut open = |seed: u8, id: usize, temp: &tempfile::TempDir| Device {
        service: ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: temp.path().join("channels"),
                vault: VaultKey::new([seed; 32]),
                device_key: key(seed),
            },
            Box::new(Ungoverned(policy.clone())),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .unwrap(),
        broker: Arc::new(Mutex::new(BrokerChannels::default())),
        endpoint: Endpoint::join(&room, room_id, id),
    };
    let mut first = open(1, 0, &dirs[0]);
    let mut second = open(2, 1, &dirs[1]);
    let outsider_dir = tempfile::tempdir().unwrap();
    let mut outsider = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: outsider_dir.path().join("channels"),
            vault: VaultKey::new([3; 32]),
            device_key: key(3),
        },
        Box::new(Ungoverned(policy.clone())),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap();

    let mut now = Instant::now();
    assert!(
        matches!(
            first.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ),
        "a room with no governor is taken up when its readers are named"
    );
    assert!(matches!(
        outsider.access(room_id, channel, now),
        ChannelAccess::Unavailable
    ));

    // The first device founds the group after its while and is answered
    // its keys; the second is admitted once it asks.
    let access = create(&mut first, room_id, channel, now);
    assert!(
        ready_epoch(&access).is_some(),
        "the founding device is handed the channel's keys: {access:?}"
    );
    second.service.access(room_id, channel, now);
    run(&mut [&mut first, &mut second], &mut now, 5);
    assert!(second.service.existing_keys(room_id, channel).is_some());
    assert!(
        outsider.existing_keys(room_id, channel).is_none(),
        "a device outside the identity never gets the keys"
    );
    assert!(outsider.peers.is_empty());
}
