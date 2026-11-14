/// A workspace whose readers this device's directory cannot name yet, as a
/// workspace's tree does before its authority arrives: it announces the
/// change once it can.
#[derive(Clone, Default)]
struct LateReaders {
    named: Arc<std::sync::atomic::AtomicBool>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl LateReaders {
    fn name(&self) {
        self.named.store(true, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

impl KeyDirectory for LateReaders {
    fn allowed_devices(&self, _: WorkspaceId, _: u64) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.named
            .load(Ordering::SeqCst)
            .then(|| [public(21)].into_iter().collect())
    }
    fn founds(&self, _: WorkspaceId) -> bool {
        true
    }
    fn policy_epoch(&self) -> Option<u64> {
        Some(self.epoch.load(Ordering::SeqCst))
    }
}

fn late_readers_service(dir: &Path, directory: &LateReaders) -> WorkspaceKeys {
    WorkspaceKeys::open(
        KeysConfig {
            state: Arc::new(KeyDir::new(dir.join("groups"))),
            vault: VaultKey::new([21; 32]),
            signer: std::sync::Arc::new(key(21)),
            role: crate::KeyRole::Committer,
        },
        Box::new(directory.clone()),
        Arc::new(Gates::default()),
    )
    .unwrap()
}

/// A route asked about before its group's readers are named is told when
/// they are, with no timer running meanwhile: the endpoint need not ask on
/// one to find out.
#[test]
fn a_route_hears_when_its_groups_readers_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateReaders::default();
    let mut service = late_readers_service(dir.path(), &directory);
    let workspace = WorkspaceId::new();
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    let mut now = Instant::now();
    assert!(matches!(
        service.access(workspace, now),
        KeyAccess::Unavailable
    ));
    assert!(
        std::mem::take(&mut service.queued),
        "a newly unopened group wakes a parked pump to arm its watch"
    );
    service.pump(&transport, now, 1_000);
    assert_eq!(service.workspace_change(workspace), 0);
    assert_eq!(
        service.next_pump_deadline(now),
        None,
        "nothing is due while the group's readers are unknown"
    );
    assert!(
        service.waits_for_policy_change(),
        "the pump waits for an announcement instead"
    );

    // An announcement that still names no readers tells no route.
    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_005);
    assert_eq!(service.workspace_change(workspace), 0);

    directory.name();
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_010);
    let told = service.workspace_change(workspace);
    assert_ne!(told, 0, "the route is told its readers are known now");
    assert!(matches!(service.access(workspace, now), KeyAccess::Waiting));
    assert!(service.unopened.is_empty());

    // The opened group's own passes tell the route what they change; once
    // settled, an announcement of the readers adds nothing.
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_015);
    let settled = service.workspace_change(workspace);

    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_020);
    assert_eq!(
        service.workspace_change(workspace),
        settled,
        "an opened group is not told again"
    );
}

/// A group whose state could not be opened is retried by the pump on
/// its own backoff, each retry telling the route to ask again.
#[test]
fn a_failed_open_is_retried_on_the_pumps_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateReaders::default();
    directory.name();
    let mut service = late_readers_service(dir.path(), &directory);
    let workspace = WorkspaceId::new();
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    // The workspace's key file cannot be read: a directory is in its way.
    let blocker = dir.path().join("groups").join(format!("{workspace}.peer"));
    std::fs::create_dir_all(&blocker).unwrap();
    let now = Instant::now();
    assert!(matches!(
        service.access(workspace, now),
        KeyAccess::Unavailable
    ));
    service.pump(&transport, now, 1_000);
    let due = service
        .next_pump_deadline(now)
        .expect("the failed open is due again");
    assert_eq!(due, now.checked_add(REQUEST_FIRST).unwrap());
    assert_eq!(service.workspace_change(workspace), 0);
    service.pump(&transport, due, 1_002);
    let told = service.workspace_change(workspace);
    assert_ne!(told, 0, "the retry tells the route to ask again");
    assert_eq!(
        service.next_pump_deadline(due),
        Some(due.checked_add(REQUEST_FIRST * 2).unwrap()),
        "and backs off"
    );

    std::fs::remove_dir(&blocker).unwrap();
    assert!(matches!(service.access(workspace, due), KeyAccess::Waiting));
    assert!(service.unopened.is_empty(), "an open that succeeds settles");
}
