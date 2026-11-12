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

impl ChannelDirectory for LateReaders {
    fn allowed_devices(
        &self,
        _: WorkspaceId,
        _: ChannelId,
        _: Option<&ChannelRoute>,
        _: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.named
            .load(Ordering::SeqCst)
            .then(|| [public(21)].into_iter().collect())
    }
    /// Each channel here is a reader set of its own.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        _: u64,
    ) -> Option<ChannelId> {
        Some(route.channel(&workspace.to_string()))
    }
    fn policy_epoch(&self) -> Option<u64> {
        Some(self.epoch.load(Ordering::SeqCst))
    }
}

fn late_readers_service(dir: &Path, directory: &LateReaders) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(ChannelsDir::new(dir.join("channels"))),
            vault: VaultKey::new([21; 32]),
            signer: std::sync::Arc::new(key(21)),
            role: crate::Role::Participant,
        },
        Box::new(directory.clone()),
        Arc::new(Gates::default()),
    )
    .unwrap()
}

/// A route asked about before its channel's readers are named is told when
/// they are, with no timer running meanwhile: the endpoint need not ask on
/// one to find out.
#[test]
fn a_route_hears_when_its_channels_readers_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateReaders::default();
    let mut service = late_readers_service(dir.path(), &directory);
    let (workspace, channel) = (WorkspaceId::new(), ChannelId::new());
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    let mut now = Instant::now();
    assert!(matches!(
        service.access(workspace, channel, now),
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
        "nothing is due while the channel's readers are unknown"
    );
    assert!(
        service.waits_for_policy_change(),
        "the pump waits for an announcement instead"
    );

    // An announcement that still names no readers tells no route.
    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_005);
    assert_eq!(service.channel_change(channel), 0);

    directory.name();
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_010);
    let told = service.channel_change(channel);
    assert_ne!(told, 0, "the route is told its readers are known now");
    assert!(matches!(
        service.access(workspace, channel, now),
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty());

    // The opened channel's own passes tell the route what they change; once
    // settled, an announcement of the readers adds nothing.
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_015);
    let settled = service.channel_change(channel);

    directory.epoch.fetch_add(1, Ordering::SeqCst);
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_020);
    assert_eq!(
        service.channel_change(channel),
        settled,
        "an opened channel is not told again"
    );
}

/// A channel whose state could not be opened is retried by the pump on
/// its own backoff, each retry telling the route to ask again.
#[test]
fn a_failed_open_is_retried_on_the_pumps_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateReaders::default();
    directory.name();
    let mut service = late_readers_service(dir.path(), &directory);
    let (workspace, channel) = (WorkspaceId::new(), ChannelId::new());
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    // The workspace's state directory cannot be created: a file is in its way.
    let blocker = dir.path().join("channels").join(workspace.to_string());
    std::fs::write(&blocker, b"").unwrap();
    let now = Instant::now();
    assert!(matches!(
        service.access(workspace, channel, now),
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
        service.access(workspace, channel, due),
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty(), "an open that succeeds settles");
}

/// A workspace whose directory names a channel's readers only from the
/// channel's record, as a runtime's own governance does: a route proves the
/// record, and a channel asked about without one stays unnamed. It announces
/// its changes when [`Self::announcing`], and names them by time alone when
/// quiet.
#[derive(Clone, Default)]
struct RoutedReaders {
    named: Arc<std::sync::atomic::AtomicBool>,
    /// The announcement epoch, or `None` for a directory that announces
    /// nothing and is re-checked on the pump's interval.
    epoch: Option<Arc<std::sync::atomic::AtomicU64>>,
}

impl RoutedReaders {
    fn announcing() -> Self {
        Self {
            named: Arc::default(),
            epoch: Some(Arc::default()),
        }
    }

    fn name(&self) {
        self.named.store(true, Ordering::SeqCst);
        if let Some(epoch) = &self.epoch {
            epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl ChannelDirectory for RoutedReaders {
    fn allowed_devices(
        &self,
        _: WorkspaceId,
        _: ChannelId,
        route: Option<&ChannelRoute>,
        _: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        (self.named.load(Ordering::SeqCst) && route.is_some())
            .then(|| [public(21)].into_iter().collect())
    }
    /// Each channel here is a reader set of its own.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        _: u64,
    ) -> Option<ChannelId> {
        Some(route.channel(&workspace.to_string()))
    }
    fn policy_epoch(&self) -> Option<u64> {
        self.epoch
            .as_ref()
            .map(|epoch| epoch.load(Ordering::SeqCst))
    }
}

fn routed_service(dir: &Path, directory: &RoutedReaders) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(ChannelsDir::new(dir.join("channels"))),
            vault: VaultKey::new([21; 32]),
            signer: std::sync::Arc::new(key(21)),
            role: crate::Role::Participant,
        },
        Box::new(directory.clone()),
        Arc::new(Gates::default()),
    )
    .unwrap()
}

/// A record's channel is named from its record, which only the endpoint that
/// asks about it carries: asked before the workspace names its readers, the
/// route hears when it does, and the endpoint's next ask takes the channel
/// up. The re-check asks with the route the endpoint gave, since a channel
/// id alone names no record.
#[test]
fn a_routed_channel_opens_once_its_record_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let directory = RoutedReaders::announcing();
    let mut service = routed_service(dir.path(), &directory);
    let workspace = WorkspaceId::new();
    let route = ChannelRoute::new("so.tilo.notes", "minutes");
    let channel = route.channel(&workspace.to_string());
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    let mut now = Instant::now();
    assert!(matches!(
        service.access_record(workspace, &route, now).1,
        ChannelAccess::Unavailable
    ));
    service.pump(&transport, now, 1_000);
    assert_eq!(service.channel_change(channel), 0);
    assert!(
        service.waits_for_policy_change(),
        "the pump waits for the workspace to name the record's readers"
    );

    directory.name();
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_005);
    assert_ne!(
        service.channel_change(channel),
        0,
        "the route is told its record is nameable now"
    );
    assert!(matches!(
        service.access_record(workspace, &route, now).1,
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty());
}

/// A directory that announces nothing is re-checked on the pump's interval:
/// a routed channel whose record its workspace names only later opens then,
/// so an endpoint already told its channel is unavailable hears it is not,
/// without asking again.
#[test]
fn a_routed_channel_opens_on_the_interval_without_announcements() {
    let dir = tempfile::tempdir().unwrap();
    let directory = RoutedReaders::default();
    let mut service = routed_service(dir.path(), &directory);
    let workspace = WorkspaceId::new();
    let route = ChannelRoute::new("so.tilo.notes", "minutes");
    let channel = route.channel(&workspace.to_string());
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    let now = Instant::now();
    assert!(matches!(
        service.access_record(workspace, &route, now).1,
        ChannelAccess::Unavailable
    ));
    service.pump(&transport, now, 1_000);
    assert_eq!(service.channel_change(channel), 0);

    directory.name();
    let due = service
        .next_pump_deadline(now)
        .expect("the interval re-check is due");
    assert_eq!(due, now.checked_add(RECHECK_INTERVAL).unwrap());
    service.pump(&transport, due, 1_001);
    assert_ne!(
        service.channel_change(channel),
        0,
        "the interval re-check tells the route its record is nameable"
    );
    assert!(matches!(
        service.access_record(workspace, &route, due).1,
        ChannelAccess::Waiting
    ));
    assert!(service.unopened.is_empty());
}

/// A storage unit told of a change of its own — its readers named — still
/// hears every change of the scope that carries it after: a reader that took
/// the unit's keys once takes the scope's next epoch too, instead of keeping
/// keys nobody seals under any more.
#[test]
fn a_unit_told_of_its_own_change_still_hears_its_scope_move_after() {
    let dir = tempfile::tempdir().unwrap();
    let directory = LateReaders::default();
    let mut service = late_readers_service(dir.path(), &directory);
    let (workspace, unit, scope) = (WorkspaceId::new(), ChannelId::new(), ChannelId::new());
    let transport = Endpoint::join(&Arc::new(Workspace::default()), workspace, 0);
    let mut now = Instant::now();
    assert!(matches!(
        service.access(workspace, unit, now),
        ChannelAccess::Unavailable
    ));
    service.pump(&transport, now, 1_000);
    directory.name();
    now = now.checked_add(Duration::from_secs(5)).unwrap();
    service.pump(&transport, now, 1_005);
    let told = service.channel_change(unit);
    assert_ne!(told, 0, "the unit is told its readers are known");

    // The unit turns out to be carried by the workspace's scope, whose
    // epoch then moves, as when a member is admitted to it.
    service.units.insert(unit, scope);
    service.bump(scope);
    assert!(
        service.channel_change(unit) > told,
        "the scope's new epoch reaches the unit's readers"
    );
    assert_eq!(service.channel_change(unit), service.generation());
}
