use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn the_channel_state_key_file_is_created_once_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let room = RoomId::new();
    let path = directory.path().join(CHANNEL_STATE_KEY);
    vault_key(directory.path(), room, IdentityStorage::Files).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert_eq!(first.len(), 32);
    vault_key(directory.path(), room, IdentityStorage::Files).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        first,
        "an existing key is kept"
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the key is owner-only");
    std::fs::write(&path, b"short").unwrap();
    assert!(
        vault_key(directory.path(), room, IdentityStorage::Files).is_err(),
        "a damaged key is refused, never replaced"
    );
}

/// The governance pump sleeps while no channel is live and runs as soon as
/// there is something to send or receive.
mod pump {
    use super::*;
    use collab_types::ChannelId;
    use std::time::Instant;
    use tilo_action_broker::{
        channel_encryption::testing::{Directory, Endpoint, Policy, Room, key, public},
        collab_core::BrokerChannels,
    };

    /// Device `seed`'s channel service in a room device `steward` governs.
    fn service((seed, steward): (u8, u8), dir: &Path) -> ChannelEncryption {
        ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: dir.join(CHANNEL_STATE),
                vault: VaultKey::new([seed; 32]),
                device_key: key(seed),
            },
            Box::new(Directory {
                governs: seed == steward,
                steward: public(steward),
                policy: Policy::default(),
            }),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )
        .unwrap()
    }

    /// A pump on endpoint `id` of `room`, woken as `channels::open` wires it.
    fn parked(
        room: &Arc<Room>,
        room_id: RoomId,
        id: usize,
        service: ChannelEncryption,
    ) -> (ChannelKeyAccess, Arc<PumpWake>, Pump) {
        let wake = Arc::new(PumpWake::default());
        let poke = Arc::clone(&wake);
        let access = ChannelKeyAccess::with_wake(service, Arc::new(move || poke.poke()));
        let pump = spawn_pump(
            access.clone(),
            Arc::new(Endpoint::join(room, room_id, id)),
            Arc::clone(&wake),
        )
        .unwrap();
        (access, wake, pump)
    }

    fn queued(room: &Room, id: usize) -> usize {
        room.queues.lock().unwrap().get(&id).map_or(0, Vec::len)
    }

    /// Wait up to half the live interval for `done`: a pump that only ran
    /// on a timer would miss it.
    fn promptly(mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < LIVE_INTERVAL.div_f64(2.0) {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    fn a_first_channel_is_announced_without_waiting_for_a_timer() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let dir = tempfile::tempdir().unwrap();
        let listener = Endpoint::join(&room, room_id, 1);
        let (access, _, _pump) = parked(&room, room_id, 0, service((1, 1), dir.path()));
        assert!(!access.has_live_channels());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(queued(&room, listener.id), 0);

        access.access(room_id, ChannelId::new());
        assert!(
            promptly(|| queued(&room, listener.id) > 0),
            "the parked pump never announced the new channel's genesis"
        );
    }

    #[test]
    fn arriving_governance_wakes_a_parked_pump() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let steward_dir = tempfile::tempdir().unwrap();
        let member_dir = tempfile::tempdir().unwrap();
        let (_access, wake, _pump) = parked(&room, room_id, 0, service((2, 1), member_dir.path()));
        std::thread::sleep(Duration::from_millis(50));

        let steward = Endpoint::join(&room, room_id, 1);
        let mut governor = service((1, 1), steward_dir.path());
        governor.access(room_id, ChannelId::new(), Instant::now());
        governor.pump(&steward, Instant::now(), 1_000);
        assert!(queued(&room, 0) > 0, "the steward announced nothing");
        std::thread::sleep(Duration::from_millis(50));
        assert!(queued(&room, 0) > 0, "the parked pump ran without a poke");

        wake.poke();
        assert!(
            promptly(|| queued(&room, 0) == 0),
            "the poked pump left the arrived governance unread"
        );
    }

    /// Linux only: it reads the thread's context switches from
    /// `/proc/self/task`.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_idle_pump_with_no_channel_is_never_scheduled() {
        fn threads() -> BTreeSet<String> {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        }
        fn switches(tid: &str) -> u64 {
            std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
                .unwrap()
                .lines()
                .filter(|line| line.contains("ctxt_switches"))
                .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
                .sum()
        }
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let dir = tempfile::tempdir().unwrap();
        let before = threads();
        let (access, _, _pump) = parked(&room, room_id, 0, service((1, 1), dir.path()));
        let started = Instant::now();
        // The kernel truncates thread names to 15 bytes.
        let tid = loop {
            let named = threads()
                .difference(&before)
                .find(|tid| {
                    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                        .is_ok_and(|name| name.trim() == "tilo-runtime-ch")
                })
                .cloned();
            if let Some(tid) = named {
                break tid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no pump thread");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!access.has_live_channels());
        std::thread::sleep(Duration::from_millis(50));
        let settled = switches(&tid);
        std::thread::sleep(Duration::from_millis(400));
        let woke = switches(&tid).saturating_sub(settled);
        assert_eq!(woke, 0, "the idle channel pump woke {woke} times in 400 ms");
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use tilo_secrets::{Protection, Secret, SecretBackend, SecretError};

    fn stored(room: RoomId) -> Option<Vec<u8>> {
        tilo_secrets::SecretStore::for_app("so.tilo.runtime")
            .get_blocking(&format!("channel-state-v1.{room}"))
            .unwrap()
            .map(|secret| secret.expose().to_vec())
    }

    #[test]
    fn a_key_file_moves_into_the_keychain_once_and_no_file_is_written() {
        let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CHANNEL_STATE_KEY);

        let fresh = RoomId::new();
        vault_key(directory.path(), fresh, IdentityStorage::Platform).unwrap();
        assert!(!path.exists(), "a new key was written to a file");
        let first = stored(fresh).unwrap();
        vault_key(directory.path(), fresh, IdentityStorage::Platform).unwrap();
        assert_eq!(stored(fresh).unwrap(), first, "an existing key is kept");

        let migrated = RoomId::new();
        vault_key(directory.path(), migrated, IdentityStorage::Files).unwrap();
        let file_key = std::fs::read(&path).unwrap();
        vault_key(directory.path(), migrated, IdentityStorage::Platform).unwrap();
        assert_eq!(stored(migrated).unwrap(), file_key);
        assert!(!path.exists(), "the key file stayed behind");
    }

    struct Locked;

    impl SecretBackend for Locked {
        fn protection(&self) -> Protection {
            Protection::OsKeychain
        }
        fn get(&self, _: &str, _: &str) -> Result<Option<Secret>, SecretError> {
            Err(SecretError::Denied)
        }
        fn put(&self, _: &str, _: &str, _: &[u8], _: &str) -> Result<(), SecretError> {
            Err(SecretError::Denied)
        }
        fn delete(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Denied)
        }
    }

    #[test]
    fn a_conflicting_or_refusing_keychain_fails_closed_and_keeps_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CHANNEL_STATE_KEY);
        let room = RoomId::new();
        {
            let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
            vault_key(directory.path(), room, IdentityStorage::Platform).unwrap();
            std::fs::write(&path, [9; 32]).unwrap();
            let error = vault_key(directory.path(), room, IdentityStorage::Platform).unwrap_err();
            assert!(error.to_string().contains("different"), "{error}");
        }
        {
            let _keychain = tilo_secrets::scope_thread(std::sync::Arc::new(Locked));
            let error = vault_key(directory.path(), room, IdentityStorage::Platform).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(std::fs::read(&path).unwrap(), [9; 32]);
    }
}
