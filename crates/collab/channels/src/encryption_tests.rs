use super::*;
use crate::gates::Gates;
use crate::testing::*;
use collab_types::{ArtifactId, ContentRetention};
use std::path::Path;
use std::sync::atomic::Ordering;

struct Device {
    service: ChannelEncryption,
    gates: Arc<Gates>,
    endpoint: Endpoint,
}

/// Device `seed` of workspace `workspace_id`, with its state under `dir`
/// and transport endpoint `id` of `workspace`.
fn device(
    workspace: &Arc<Workspace>,
    workspace_id: WorkspaceId,
    id: usize,
    seed: u8,
    policy: &Policy,
    dir: &Path,
) -> Device {
    sealed_device(workspace, workspace_id, id, (seed, seed), policy, dir)
}

/// [`device`] whose state is sealed under the vault of `vault` and whose
/// device key is `seed`'s: a machine copied from another keeps the person's
/// vault and gets a key of its own.
fn sealed_device(
    workspace: &Arc<Workspace>,
    workspace_id: WorkspaceId,
    id: usize,
    (vault, seed): (u8, u8),
    policy: &Policy,
    dir: &Path,
) -> Device {
    workspace.queues.lock().unwrap().entry(id).or_default();
    let gates = Arc::new(Gates::default());
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(ChannelsDir::new(dir.join("channels"))),
            vault: VaultKey::new([vault; 32]),
            signer: std::sync::Arc::new(key(seed)),
            role: crate::Role::Participant,
        },
        Box::new(Directory {
            policy: policy.clone(),
        }),
        Arc::clone(&gates) as Arc<dyn ChannelGates>,
    )
    .unwrap();
    Device {
        service,
        gates,
        endpoint: Endpoint {
            workspace: Arc::clone(workspace),
            id,
            workspace_id,
        },
    }
}

/// Run every device's protocol for `seconds` of simulated time.
fn run(devices: &mut [&mut Device], now: &mut Instant, seconds: u64) {
    for _ in 0..seconds.saturating_mul(4) {
        *now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in devices.iter_mut() {
            device.service.pump(&device.endpoint, *now, 1_000);
        }
    }
}

/// `device` opens `channel` and creates its group, as it does once the
/// workspace's host said it carries no genesis of it.
fn create(
    device: &mut Device,
    workspace_id: WorkspaceId,
    channel: ChannelId,
    now: Instant,
) -> ChannelAccess {
    device.service.access(workspace_id, channel, now);
    if let Some(entry) = device.service.peers.get_mut(&channel) {
        entry.founding.answered = true;
    }
    device.service.found(channel, now, 1_000);
    device.service.access(workspace_id, channel, now)
}

fn ready_epoch(access: &ChannelAccess) -> Option<u64> {
    match access {
        ChannelAccess::Ready(keys) => Some(keys.epoch()),
        _ => None,
    }
}

fn refuses_plaintext(device: &Device, channel: ChannelId) -> bool {
    !device
        .gates
        .allows_document(&device.endpoint.workspace_id.to_string(), channel, None)
}

#[test]
fn observing_live_keys_never_opens_a_channel_or_rejoins_after_removal() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
    for service in [&owner.service, &member.service] {
        let generation = service.generation();
        assert!(service.existing_keys(workspace_id, channel).is_none());
        assert!(service.peers.is_empty());
        assert_eq!(service.generation(), generation);
    }

    // Only this explicit fixture admission creates or requests keys.
    let mut now = Instant::now();
    owner.service.access(workspace_id, channel, now);
    member.service.access(workspace_id, channel, now);
    run(&mut [&mut owner, &mut member], &mut now, 5);
    assert!(
        member
            .service
            .existing_keys(workspace_id, channel)
            .is_some()
    );
    assert!(
        member
            .service
            .existing_keys(WorkspaceId::new(), channel)
            .is_none()
    );
    assert!(
        owner
            .service
            .existing_keys(WorkspaceId::new(), channel)
            .is_none()
    );

    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut member], &mut now, 3);
    let generation = member.service.generation();
    let rejoin = member.service.peers.get(&channel).unwrap().rejoin;
    assert!(!rejoin);
    assert!(
        member
            .service
            .existing_keys(workspace_id, channel)
            .is_none()
    );
    assert_eq!(member.service.generation(), generation);
    assert!(!member.service.peers.get(&channel).unwrap().rejoin);
}

#[test]
fn opening_a_channel_wakes_a_parked_pump_and_reading_it_does_not() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let dir = tempfile::tempdir().unwrap();
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    let owner = device(
        &workspace,
        workspace_id,
        0,
        1,
        &Policy::readers([1, 2]),
        dir.path(),
    );
    let access = ChannelKeyAccess::with_wake(
        owner.service,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let channel = ChannelId::new();
    access.access(workspace_id, channel);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "a new group has a genesis to announce"
    );
    access.access(workspace_id, channel);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "an open channel queues nothing new"
    );

    let member_dir = tempfile::tempdir().unwrap();
    let member = device(
        &workspace,
        workspace_id,
        1,
        2,
        &Policy::readers([1, 2]),
        member_dir.path(),
    );
    let member_wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&member_wakes);
    let member = ChannelKeyAccess::with_wake(
        member.service,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert!(matches!(
        member.access(workspace_id, channel).0,
        ChannelAccess::Waiting
    ));
    assert_eq!(
        member_wakes.load(Ordering::SeqCst),
        1,
        "a new member has an access request to make"
    );
    member.access(workspace_id, channel);
    assert_eq!(member_wakes.load(Ordering::SeqCst), 1);
}

/// A directory whose units share the workspace's key scope, but for the
/// ones its policy gives readers of their own.
struct ScopedDirectory {
    readers: Directory,
    narrowed: Narrowing,
}

/// The units whose readers a Read Deny narrows until the test lifts it: a
/// unit whose record was sealed before key scopes is emulated by one held
/// narrowed first and carried by the workspace's scope after.
#[derive(Clone, Default)]
struct Narrowing {
    units: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
}

impl Narrowing {
    fn of(workspace: WorkspaceId, routes: &[ChannelRoute]) -> Self {
        let mut units = std::collections::HashSet::new();
        for route in routes {
            units.insert(route.channel(&workspace.to_string()));
        }
        Self {
            units: Arc::new(Mutex::new(units)),
        }
    }

    /// Let the workspace's scope carry `route`'s unit from now on.
    fn lift(&self, workspace: WorkspaceId, route: &ChannelRoute) {
        self.units
            .lock()
            .unwrap()
            .remove(&route.channel(&workspace.to_string()));
    }
}

impl ChannelDirectory for ScopedDirectory {
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: u64,
    ) -> Option<std::collections::BTreeSet<Ed25519PublicKey>> {
        self.readers.allowed_devices(workspace, channel, route, now)
    }
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        _: u64,
    ) -> Option<ChannelId> {
        let unit = route.channel(&workspace.to_string());
        Some(if self.narrowed.units.lock().unwrap().contains(&unit) {
            unit
        } else {
            collab_types::workspace_scope(&workspace.to_string())
        })
    }
}

/// [`device`] of a workspace whose units share one key scope.
fn scoped_device(
    workspace: &Arc<Workspace>,
    workspace_id: WorkspaceId,
    id: usize,
    seed: u8,
    policy: &Policy,
    narrowed: &Narrowing,
    dir: &Path,
) -> Device {
    let gates = Arc::new(Gates::default());
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            state: Arc::new(ChannelsDir::new(dir.join("channels"))),
            vault: VaultKey::new([seed; 32]),
            signer: std::sync::Arc::new(key(seed)),
            role: crate::Role::Participant,
        },
        Box::new(ScopedDirectory {
            readers: Directory {
                policy: policy.clone(),
            },
            narrowed: narrowed.clone(),
        }),
        Arc::clone(&gates) as Arc<dyn ChannelGates>,
    )
    .unwrap();
    Device {
        service,
        gates,
        endpoint: Endpoint::join(workspace, workspace_id, id),
    }
}

/// Units of one reader set share one group: a device takes part once for
/// all of them, its keys answer each unit's payloads under that unit's own
/// channel, and a membership change costs one commit for the whole set.
#[test]
fn units_of_one_reader_set_share_one_group_and_one_commit() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let notes = ChannelRoute::new("so.tilo.notes", "minutes");
    let payroll = ChannelRoute::new("so.tilo.sheets", "payroll");
    let scope = collab_types::workspace_scope(&workspace_id.to_string());
    let policy = Policy::readers([1]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = scoped_device(
        &workspace,
        workspace_id,
        0,
        1,
        &policy,
        &Narrowing::default(),
        owner_dir.path(),
    );
    let mut laptop = scoped_device(
        &workspace,
        workspace_id,
        1,
        2,
        &policy,
        &Narrowing::default(),
        laptop_dir.path(),
    );

    // One group, carrying both units.
    assert!(ready_epoch(&create(&mut owner, workspace_id, scope, now)).is_some());
    assert_eq!(owner.service.peers.len(), 1);
    let (unit, ChannelAccess::Ready(notes_keys)) =
        owner.service.access_record(workspace_id, &notes, now)
    else {
        panic!("the owner reads the record it opened");
    };
    assert_eq!(unit, notes.channel(&workspace_id.to_string()));
    assert_eq!(unit, notes_keys.channel_id());
    let (paid, ChannelAccess::Ready(payroll_keys)) =
        owner.service.access_record(workspace_id, &payroll, now)
    else {
        panic!("the scope carries every unit of the workspace");
    };
    assert_eq!(paid, payroll.channel(&workspace_id.to_string()));
    assert_eq!(payroll_keys.epoch(), notes_keys.epoch());
    assert_eq!(owner.service.peers.len(), 1, "no group per unit");

    // A sealed payload names its unit, and the unit's keys open it.
    let artifact =
        collab_types::document_artifact_id(&notes.app, &workspace_id.to_string(), &notes.record);
    let (metadata, sealed) = notes_keys
        .seal_content(artifact, ContentRetention::Delta, b"the minutes")
        .expect("seal");
    assert_eq!(metadata.channel, unit);
    assert_eq!(
        notes_keys
            .open_content(artifact, &metadata, &sealed)
            .expect("open")
            .as_slice(),
        b"the minutes"
    );

    // The scope's gate is what refuses a unit's cleartext: the unit has no
    // gate of its own, and its payloads are sealed with the scope's keys.
    assert!(
        !owner
            .gates
            .allows_document(&workspace_id.to_string(), unit, None)
    );
    assert!(
        owner
            .gates
            .allows_document(&workspace_id.to_string(), unit, Some(&metadata)),
        "a sealed unit payload passes a gate that requires encryption"
    );

    // The laptop is admitted once for the whole set: one commit, and both
    // units open under it.
    policy.allowed.lock().unwrap().insert(public(2));
    assert!(matches!(
        laptop.service.access_record(workspace_id, &notes, now).1,
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let epoch = ready_epoch(&owner.service.access_record(workspace_id, &notes, now).1);
    assert!(epoch.is_some());
    for route in [&notes, &payroll] {
        assert_eq!(
            ready_epoch(&laptop.service.access_record(workspace_id, route, now).1),
            epoch
        );
    }

    // A member leaving is one commit for both units, and both units hear
    // of it.
    let notes_change = owner
        .service
        .channel_change(notes.channel(&workspace_id.to_string()));
    assert!(policy.allowed.lock().unwrap().remove(&public(2)));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let after = ready_epoch(&owner.service.access_record(workspace_id, &notes, now).1);
    assert!(after > epoch, "the removal moved the scope's epoch");
    assert!(
        owner
            .service
            .channel_change(payroll.channel(&workspace_id.to_string()))
            > notes_change,
        "a unit's wait follows the scope it is carried by"
    );
    assert!(matches!(
        laptop.service.access_record(workspace_id, &notes, now).1,
        ChannelAccess::Removed
    ));
}

/// A unit whose readers the policy narrows keeps a group of its own: its
/// keys never reach a device the workspace's scope carries.
#[test]
fn a_narrowed_unit_seals_under_its_own_group() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let open = ChannelRoute::new("so.tilo.notes", "minutes");
    let payroll = ChannelRoute::new("so.tilo.sheets", "payroll");
    let scope = collab_types::workspace_scope(&workspace_id.to_string());
    let policy = Policy::readers([1, 2]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = scoped_device(
        &workspace,
        workspace_id,
        0,
        1,
        &policy,
        &Narrowing::of(workspace_id, std::slice::from_ref(&payroll)),
        owner_dir.path(),
    );
    let mut laptop = scoped_device(
        &workspace,
        workspace_id,
        1,
        2,
        &policy,
        &Narrowing::of(workspace_id, std::slice::from_ref(&payroll)),
        laptop_dir.path(),
    );
    assert!(ready_epoch(&create(&mut owner, workspace_id, scope, now)).is_some());
    let unit = payroll.channel(&workspace_id.to_string());
    assert!(ready_epoch(&create(&mut owner, workspace_id, unit, now)).is_some());
    assert_eq!(
        owner.service.peers.len(),
        2,
        "the scope and the narrowed record's own group"
    );
    assert!(matches!(
        laptop.service.access_record(workspace_id, &payroll, now).1,
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    assert!(matches!(
        laptop.service.access_record(workspace_id, &open, now).1,
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let (_, ChannelAccess::Ready(open_keys)) =
        laptop.service.access_record(workspace_id, &open, now)
    else {
        panic!("the workspace's scope carries the open record");
    };
    let (paid, ChannelAccess::Ready(payroll_keys)) =
        laptop.service.access_record(workspace_id, &payroll, now)
    else {
        panic!("the narrowed record has a group of its own");
    };
    assert_eq!(paid, unit);
    assert_ne!(payroll_keys.channel_id(), open_keys.channel_id());
    assert_ne!(
        payroll_keys.channel_id(),
        open_keys.channel_id(),
        "the narrowed record's group stands on its own"
    );
    assert_eq!(
        laptop.service.peers.len(),
        2,
        "the laptop takes part in both groups"
    );
}

/// A unit sealed before key scopes under a group of its own, whose readers
/// are now the workspace's participants. The holder of the old group hands
/// its keys to the workspace's scope, so a device added later reads the
/// unit's whole history — through the hand-over's commit or through the key
/// history of its admission — and the old holder may be offline from then
/// on.
struct LegacyUnit {
    workspace: Arc<Workspace>,
    workspace_id: WorkspaceId,
    route: ChannelRoute,
    unit: ChannelId,
    scope: ChannelId,
    policy: Policy,
    narrowing: Narrowing,
    owner: Device,
    /// Kept for as long as the owner runs, so its state has somewhere to go.
    _owner_dir: tempfile::TempDir,
    laptop_dir: tempfile::TempDir,
    tablet_dir: tempfile::TempDir,
    artifact: ArtifactId,
    metadata: EncryptionMetadata,
    sealed: Vec<u8>,
    now: Instant,
}

impl LegacyUnit {
    /// The owner's group for the unit and the history sealed under it, from
    /// before the workspace had a scope; the other devices are still kept
    /// out of the record.
    fn sealed() -> Self {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let route = ChannelRoute::new("so.tilo.notes", "minutes");
        let unit = route.channel(&workspace_id.to_string());
        let scope = collab_types::workspace_scope(&workspace_id.to_string());
        let policy = Policy::readers([1, 2, 3]);
        policy
            .withheld
            .lock()
            .unwrap()
            .insert(unit, [public(2), public(3)].into());
        let narrowing = Narrowing::of(workspace_id, std::slice::from_ref(&route));
        let (owner_dir, laptop_dir, tablet_dir) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let now = Instant::now();
        let mut owner = scoped_device(
            &workspace,
            workspace_id,
            0,
            1,
            &policy,
            &narrowing,
            owner_dir.path(),
        );
        assert!(ready_epoch(&create(&mut owner, workspace_id, unit, now)).is_some());
        let (_, ChannelAccess::Ready(old)) = owner.service.access_record(workspace_id, &route, now)
        else {
            panic!("the owner reads its own record");
        };
        let artifact = collab_types::document_artifact_id(
            &route.app,
            &workspace_id.to_string(),
            &route.record,
        );
        let (metadata, sealed) = old
            .seal_content(artifact, ContentRetention::Delta, b"before key scopes")
            .unwrap();
        Self {
            workspace,
            workspace_id,
            route,
            unit,
            scope,
            policy,
            narrowing,
            owner,
            _owner_dir: owner_dir,
            laptop_dir,
            tablet_dir,
            artifact,
            metadata,
            sealed,
            now,
        }
    }

    /// A device of the workspace that holds nothing of the unit's old group:
    /// the laptop, or the tablet.
    fn device(&self, id: usize, seed: u8, dir: &Path) -> Device {
        scoped_device(
            &self.workspace,
            self.workspace_id,
            id,
            seed,
            &self.policy,
            &self.narrowing,
            dir,
        )
    }

    /// What `device` reads of the unit's old history, through the unit's own
    /// channel as a unit of the scope.
    fn opens(&self, device: &mut Device) -> Option<Vec<u8>> {
        device
            .service
            .access_record(self.workspace_id, &self.route, self.now)
            .1
            .keys()
            .open_content(self.artifact, &self.metadata, &self.sealed)
            .ok()
            .map(|plain| plain.to_vec())
    }

    /// The workspace's scope is established while the unit is still denied to
    /// the others, and the laptop joins it holding none of the unit's keys.
    fn scope_with(&mut self, laptop: &mut Device) {
        assert!(
            ready_epoch(&create(
                &mut self.owner,
                self.workspace_id,
                self.scope,
                self.now
            ))
            .is_some()
        );
        assert!(matches!(
            laptop
                .service
                .access(self.workspace_id, self.scope, self.now),
            ChannelAccess::Waiting
        ));
        run(&mut [&mut self.owner, laptop], &mut self.now, 8);
        let ChannelAccess::Ready(keys) =
            laptop
                .service
                .access(self.workspace_id, self.scope, self.now)
        else {
            panic!("the laptop joined the scope");
        };
        assert!(
            keys.for_unit(self.unit)
                .open_content(self.artifact, &self.metadata, &self.sealed)
                .is_err(),
            "the scope's keys open nothing the old group sealed, yet"
        );
    }

    /// The unit reads to every participant now: the owner hands the old
    /// group's keys to the scope — one commit, and no more however often the
    /// pass runs — and the laptop reads what the old group sealed.
    fn hand_over(&mut self, laptop: &mut Device) {
        self.policy.withheld.lock().unwrap().remove(&self.unit);
        self.narrowing.lift(self.workspace_id, &self.route);
        assert!(matches!(
            self.owner
                .service
                .access_record(self.workspace_id, &self.route, self.now)
                .1,
            ChannelAccess::Ready(_)
        ));
        run(&mut [&mut self.owner, laptop], &mut self.now, 4);
        let handed_over = ready_epoch(
            &laptop
                .service
                .access_record(self.workspace_id, &self.route, self.now)
                .1,
        )
        .expect("the hand-over leaves the laptop reading");
        assert!(
            !self.owner.service.peers[&self.scope]
                .peer
                .holds_unshared_keys(),
            "the scope took the keys over"
        );
        for _ in 0..2 {
            run(&mut [&mut self.owner, laptop], &mut self.now, 4);
            assert_eq!(
                ready_epoch(
                    &laptop
                        .service
                        .access_record(self.workspace_id, &self.route, self.now)
                        .1
                ),
                Some(handed_over),
                "a hand-over happens once"
            );
        }
        assert_eq!(
            self.opens(laptop).as_deref(),
            Some(b"before key scopes".as_slice())
        );
    }
}

#[test]
fn a_device_added_after_key_scopes_reads_a_legacy_units_whole_history() {
    let mut unit = LegacyUnit::sealed();
    let mut laptop = unit.device(1, 2, unit.laptop_dir.path());
    unit.scope_with(&mut laptop);
    // The laptop joined the scope before the hand-over and reads the old
    // history from the commit that carried the keys.
    unit.hand_over(&mut laptop);

    // A tablet admitted after the hand-over receives them with its admission.
    let mut tablet = unit.device(2, 3, unit.tablet_dir.path());
    assert!(matches!(
        tablet
            .service
            .access_record(unit.workspace_id, &unit.route, unit.now)
            .1,
        ChannelAccess::Waiting
    ));
    run(
        &mut [&mut unit.owner, &mut laptop, &mut tablet],
        &mut unit.now,
        8,
    );
    assert_eq!(
        unit.opens(&mut tablet).as_deref(),
        Some(b"before key scopes".as_slice()),
        "a device admitted after the hand-over reads the whole history"
    );

    // The holder of the old group goes offline: the keys are the scope's now,
    // and survive a restart of a device that never held that group.
    run(&mut [&mut laptop, &mut tablet], &mut unit.now, 4);
    assert_eq!(
        unit.opens(&mut tablet).as_deref(),
        Some(b"before key scopes".as_slice())
    );
    drop(tablet);
    let mut restarted = unit.device(2, 3, unit.tablet_dir.path());
    assert_eq!(
        unit.opens(&mut restarted).as_deref(),
        Some(b"before key scopes".as_slice())
    );
}

/// A unit whose readers a Read Deny narrows keeps its keys out of the
/// workspace's scope, however many hand-over passes run: the denied device
/// never reads what the unit's own group sealed.
#[test]
fn a_read_deny_keeps_a_legacy_units_keys_out_of_the_scope() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let payroll = ChannelRoute::new("so.tilo.sheets", "payroll");
    let open = ChannelRoute::new("so.tilo.notes", "minutes");
    let unit = payroll.channel(&workspace_id.to_string());
    let scope = collab_types::workspace_scope(&workspace_id.to_string());
    let policy = Policy::readers([1, 2]);
    policy
        .withheld
        .lock()
        .unwrap()
        .insert(unit, [public(2)].into());
    let narrowing = Narrowing::of(workspace_id, std::slice::from_ref(&payroll));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = scoped_device(
        &workspace,
        workspace_id,
        0,
        1,
        &policy,
        &narrowing,
        owner_dir.path(),
    );
    let mut laptop = scoped_device(
        &workspace,
        workspace_id,
        1,
        2,
        &policy,
        &narrowing,
        laptop_dir.path(),
    );

    // The denied record sealed under its own group before key scopes.
    assert!(ready_epoch(&create(&mut owner, workspace_id, unit, now)).is_some());
    let (_, ChannelAccess::Ready(old)) = owner.service.access_record(workspace_id, &payroll, now)
    else {
        panic!("the owner reads the record the deny keeps from the laptop");
    };
    let artifact = collab_types::document_artifact_id(
        &payroll.app,
        &workspace_id.to_string(),
        &payroll.record,
    );
    let (old_metadata, old_sealed) = old
        .seal_content(artifact, ContentRetention::Delta, b"payroll")
        .unwrap();

    // The workspace's scope exists, the laptop joins it, and hand-over passes
    // run for as long as a unit the scope carries would be handed over.
    assert!(ready_epoch(&create(&mut owner, workspace_id, scope, now)).is_some());
    assert!(matches!(
        laptop.service.access_record(workspace_id, &open, now).1,
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 12);
    let (_, ChannelAccess::Ready(scope_keys)) =
        laptop.service.access_record(workspace_id, &open, now)
    else {
        panic!("the laptop reads the record its role allows");
    };
    assert!(
        !owner.service.peers[&scope].peer.holds_unshared_keys(),
        "the denied unit's keys are not queued for the scope"
    );
    assert!(
        scope_keys
            .open_content(artifact, &old_metadata, &old_sealed)
            .is_err(),
        "a denied device's scope keys open nothing the denied unit sealed"
    );
    // The unit's own group still reads its history, and stays its own.
    assert!(matches!(
        owner.service.access_record(workspace_id, &payroll, now).1,
        ChannelAccess::Ready(_)
    ));
    assert!(matches!(
        laptop.service.access_record(workspace_id, &payroll, now).1,
        ChannelAccess::Unavailable
    ));
}

/// `access`, the answer for a unit, once admitted: the keys, so a test reads
/// what they open without matching the variant.
trait Keys {
    fn keys(&self) -> &ChannelContentKeys;
}

impl Keys for ChannelAccess {
    fn keys(&self) -> &ChannelContentKeys {
        match self {
            ChannelAccess::Ready(keys) => keys,
            other => panic!("expected keys, got {other:?}"),
        }
    }
}

#[test]
fn a_new_channel_is_encrypted_from_creation_and_follows_the_policy() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([1]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    assert!(
        refuses_plaintext(&owner, channel),
        "encrypted from creation"
    );
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Unavailable
    ));

    // Not named yet: nobody can admit it, and its route hears when that
    // changes.
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Unavailable
    ));

    policy.allowed.lock().unwrap().insert(public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let owner_epoch = ready_epoch(&owner.service.access(workspace_id, channel, now));
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, channel, now)),
        owner_epoch
    );
    assert!(
        refuses_plaintext(&laptop, channel),
        "gate: {:?} peer: {:?}",
        laptop.service.registered.get(&channel),
        laptop
            .service
            .peers
            .get(&channel)
            .map(|e| (e.peer.access(), e.peer.gate()))
    );

    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Removed
    ));
}

/// A participant that cannot name a channel's record (it lost the route,
/// and no grant names the record) cannot tell who may read it: it removes
/// no one, though the policy it can read names no one. Named
/// again, the group follows the policy once more.
#[test]
fn a_participant_that_cannot_name_the_record_removes_no_one() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let route = ChannelRoute::new("notes", "plan");
    let channel = route.channel(&workspace_id.to_string());
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    for device in [&mut owner, &mut laptop] {
        device.service.access(workspace_id, channel, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let epoch = ready_epoch(&laptop.service.access(workspace_id, channel, now));
    assert!(epoch.is_some());
    assert_eq!(
        ready_epoch(&owner.service.access(workspace_id, channel, now)),
        epoch
    );

    // Everyone loses the route; the record is no longer named.
    policy.unnamed.lock().unwrap().insert(channel);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert_eq!(
        ready_epoch(&owner.service.access(workspace_id, channel, now)),
        epoch
    );
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, channel, now)),
        epoch
    );

    // Named again, the group follows the policy: the laptop is removed.
    owner.service.learn_route(workspace_id, channel, &route);
    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Removed
    ));
    assert!(ready_epoch(&owner.service.access(workspace_id, channel, now)) > epoch);
}

/// A Read Deny on one record takes the denied device out of that
/// record's channel alone: a participant removes it in a new epoch there,
/// the workspace's other channels keep their epoch and the device keeps their
/// keys, and a request of the denied device never makes a participant
/// create a group for the denied record.
#[test]
fn a_device_withheld_from_one_channel_loses_only_that_channels_key() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let (payroll, notes, budget) = (ChannelId::new(), ChannelId::new(), ChannelId::new());
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    for channel in [payroll, notes] {
        owner.service.access(workspace_id, channel, now);
        laptop.service.access(workspace_id, channel, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    let notes_epoch = ready_epoch(&owner.service.access(workspace_id, notes, now));
    let payroll_epoch = ready_epoch(&owner.service.access(workspace_id, payroll, now));
    assert!(ready_epoch(&laptop.service.access(workspace_id, payroll, now)).is_some());
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, notes, now)),
        notes_epoch
    );

    policy
        .withheld
        .lock()
        .unwrap()
        .extend([payroll, budget].map(|channel| (channel, BTreeSet::from([public(2)]))));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, payroll, now),
        ChannelAccess::Removed
    ));
    let rotated = ready_epoch(&owner.service.access(workspace_id, payroll, now));
    assert!(rotated > payroll_epoch, "the denied record's key rotates");
    assert_eq!(
        ready_epoch(&owner.service.access(workspace_id, notes, now)),
        notes_epoch,
        "another record's channel keeps its epoch"
    );
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, notes, now)),
        notes_epoch
    );
    // Asking again for the denied record, or first for another denied
    // one, admits nothing and creates no group at the owner.
    laptop.service.access(workspace_id, budget, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    for channel in [payroll, budget] {
        assert!(ready_epoch(&laptop.service.access(workspace_id, channel, now)).is_none());
    }
    assert!(!owner.service.peers.contains_key(&budget));
    assert!(
        !owner.service.peers[&payroll]
            .peer
            .members()
            .contains(&public(2))
    );

    policy.withheld.lock().unwrap().clear();
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, payroll, now)),
        ready_epoch(&owner.service.access(workspace_id, payroll, now))
    );
}

/// A Read Deny on an app takes the denied device out of every channel
/// of that app whose route a participant knows (from its own endpoint or
/// from the device's routed request) in a new epoch of each, and out of
/// no other app's channel. A record of the app first asked for after the
/// Deny, and a channel asked for without a route, admit nothing and
/// create no group. A route that is not its channel's is refused. The
/// owner keeps the routes: opened again, it still withholds the app's
/// channels and admits the device to nothing of it.
#[test]
fn a_read_deny_on_an_app_withholds_every_routed_channel_of_it() {
    let workspace = Arc::new(Workspace::default());
    let id = WorkspaceId::new();
    let workspace_name = id.to_string();
    let routes = [
        ChannelRoute::new("finance", "payroll"),
        ChannelRoute::new("finance", "q3"),
        ChannelRoute::new("notes", "draft"),
    ];
    let [payroll, q3, draft] = routes.clone().map(|route| route.channel(&workspace_name));
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, id, 1, 2, &policy, laptop_dir.path());
    // The owner's own endpoint opens payroll; q3 and draft the owner
    // learns from the laptop's routed requests alone.
    owner.service.access_record(id, &routes[0], now);
    for route in &routes {
        laptop.service.access_record(id, route, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    // Whichever of the two created q3's and draft's groups, the owner
    // takes part in them too.
    for channel in [q3, draft] {
        owner.service.access(id, channel, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let epoch =
        |device: &mut Device, channel, now| ready_epoch(&device.service.access(id, channel, now));
    let before: Vec<Option<u64>> = [payroll, q3, draft]
        .into_iter()
        .map(|channel| epoch(&mut owner, channel, now))
        .collect();
    for channel in [payroll, q3, draft] {
        assert!(epoch(&mut laptop, channel, now).is_some());
        assert!(owner.service.peers[&channel].route.is_some());
    }

    policy
        .withheld_apps
        .lock()
        .unwrap()
        .insert("finance".into(), BTreeSet::from([public(2)]));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    for (index, channel) in [payroll, q3].into_iter().enumerate() {
        assert!(matches!(
            laptop.service.access(id, channel, now),
            ChannelAccess::Removed
        ));
        assert!(
            epoch(&mut owner, channel, now) > before[index],
            "the denied app's key rotates"
        );
    }
    assert_eq!(epoch(&mut owner, draft, now), before[2]);
    assert_eq!(epoch(&mut laptop, draft, now), before[2]);

    // A new finance record, a channel asked for with no route, and one
    // whose route is a forgery admit nothing and create no group.
    let budget = ChannelRoute::new("finance", "budget");
    laptop.service.access_record(id, &budget, now);
    let unrouted = ChannelRoute::new("notes", "plan").channel(&workspace_name);
    laptop.service.access(id, unrouted, now);
    let forged = ChannelRoute::new("notes", "forged").channel(&workspace_name);
    let request = member_request(&laptop_dir.path().join("forged"), forged, 2);
    owner
        .service
        .receive_request(id, request, Some(&routes[2]), now, 0);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    for channel in [budget.channel(&workspace_name), unrouted, forged] {
        assert!(!owner.service.peers.contains_key(&channel));
        assert!(epoch(&mut laptop, channel, now).is_none());
    }

    // Opened again, the owner still knows every route and admits the
    // device to nothing of the app, however often it asks.
    drop(owner);
    let mut owner = device(&workspace, id, 0, 1, &policy, owner_dir.path());
    for channel in [payroll, q3, draft] {
        assert!(owner.service.peers[&channel].route.is_some());
    }
    for route in &routes {
        laptop.service.access_record(id, route, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    for channel in [payroll, q3] {
        assert!(epoch(&mut laptop, channel, now).is_none());
        assert!(
            !owner.service.peers[&channel]
                .peer
                .members()
                .contains(&public(2))
        );
    }
    assert!(epoch(&mut laptop, draft, now).is_some());

    policy.withheld_apps.lock().unwrap().clear();
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    for channel in [payroll, q3] {
        assert_eq!(
            epoch(&mut laptop, channel, now),
            epoch(&mut owner, channel, now)
        );
    }
}

/// The access request device `seed` makes for `channel`, with its state
/// under `dir`.
fn member_request(dir: &Path, channel: ChannelId, seed: u8) -> MlsAccessRequestV1 {
    std::fs::create_dir_all(dir).unwrap();
    ChannelPeer::waiting(
        Some(std::sync::Arc::new(channel_crypto::Files::new(dir))),
        channel,
        Box::new(key(seed)),
        &VaultKey::new([2; 32]),
    )
    .unwrap()
    .access_request_now(1, false, &VaultKey::new([2; 32]))
    .unwrap()
    .unwrap()
}

/// Run every device's protocol for `seconds` of simulated time the way
/// the broker's drain thread does: a device runs a pass only
/// when a message waits for it or the wait its last pass returned has
/// run out, and otherwise stays parked. `parked` carries each device's
/// wait across calls (`None`: parked until a message); a test that calls
/// [`ChannelEncryption::access`] wakes the device as the installed pump
/// wake would. Returns the passes run.
fn run_parked(
    devices: &mut [&mut Device],
    parked: &mut [Option<Instant>],
    now: &mut Instant,
    seconds: u64,
) -> usize {
    let end = now.checked_add(Duration::from_secs(seconds)).unwrap();
    let mut passes = 0_usize;
    loop {
        let mut ran = false;
        for (device, until) in devices.iter_mut().zip(parked.iter_mut()) {
            let inbound = device
                .endpoint
                .workspace
                .queues
                .lock()
                .unwrap()
                .get(&device.endpoint.id)
                .is_some_and(|queue| !queue.is_empty());
            if !inbound && until.is_none_or(|until| until > *now) {
                continue;
            }
            device.service.pump(&device.endpoint, *now, 1_000);
            *until = wait_after_pass(device.service.next_pump_deadline(*now), *now)
                .map(|wait| now.checked_add(wait).unwrap());
            passes = passes.saturating_add(1);
            ran = true;
            assert!(passes < 10_000, "a parked pump must not spin");
        }
        if ran {
            continue;
        }
        match parked.iter().flatten().min() {
            Some(next) if *next <= end => *now = *next,
            _ => {
                *now = end;
                return passes;
            }
        }
    }
}

/// #3998: a device in a channel's group re-checks its policy once a
/// reconcile interval, whoever created the group; a device with no
/// channel has nothing to pump.
#[test]
fn only_a_group_member_or_an_unfinished_member_has_a_pump_deadline() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
    assert_eq!(owner.service.next_pump_deadline(now), None, "no channel");
    assert_eq!(member.service.next_pump_deadline(now), None, "no channel");

    owner.service.access(workspace_id, channel, now);
    member.service.access(workspace_id, channel, now);
    assert_eq!(
        owner.service.next_pump_deadline(now),
        Some(now),
        "a new group reconciles at once"
    );
    assert_eq!(
        member.service.next_pump_deadline(now),
        Some(now),
        "a new member asks at once"
    );

    run(&mut [&mut owner, &mut member], &mut now, 5);
    assert!(ready_epoch(&member.service.access(workspace_id, channel, now)).is_some());
    for device in [&owner, &member] {
        let reconcile = device.service.next_pump_deadline(now).unwrap();
        assert!(reconcile > now.checked_sub(RECONCILE_INTERVAL).unwrap());
        assert!(reconcile <= now.checked_add(RECONCILE_INTERVAL).unwrap());
    }
    assert_eq!(
        wait_after_pass(Some(now), now),
        Some(RECONCILE_INTERVAL),
        "work a pass left due is retried at the old cadence, not spun on"
    );
    assert_eq!(wait_after_pass(None, now), None);
}

/// #3998: a device waiting to be admitted to a group it did not found
/// asks again on its backoff, and is due exactly then.
#[test]
fn a_waiting_member_is_due_on_its_request_backoff() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let dir = tempfile::tempdir().unwrap();
    // The directory announces its policy changes, so nothing but the
    // request backoff is ever due here.
    let policy = Policy::readers([1, 2]);
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
        std::sync::atomic::AtomicU64::new(1),
    )));
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
    // The channel's group exists: this device saw its genesis, so it
    // waits to be admitted instead of founding one of its own.
    member.service.groups_seen.insert(channel);
    let now = Instant::now();
    assert!(matches!(
        member.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    member.service.pump(&member.endpoint, now, 1_000);
    assert_eq!(
        member.service.next_pump_deadline(now),
        Some(now.checked_add(REQUEST_FIRST).unwrap())
    );
    let later = now.checked_add(REQUEST_FIRST).unwrap();
    member.service.pump(&member.endpoint, later, 1_000);
    assert_eq!(
        member.service.next_pump_deadline(later),
        Some(later.checked_add(REQUEST_FIRST.saturating_mul(2)).unwrap())
    );
}

/// A peer another member's access request opened answers that member
/// and asks nothing itself. The first read that wants the channel asks
/// for it: the read queues the pass, so a parked pump sends the request
/// instead of sleeping with the keys never asked for.
#[test]
fn a_read_that_first_wants_a_channel_queues_the_pass_that_asks() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([1, 2]);
    let dir = tempfile::tempdir().unwrap();
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
    // The group exists; this device only learns of it on admission.
    member.service.groups_seen.insert(channel);
    let now = Instant::now();
    let request = member_request(&dir.path().join("asking"), channel, 1);
    member
        .service
        .receive_request(workspace_id, request, None, now, 1_000);
    {
        let entry = member
            .service
            .peers
            .get_mut(&channel)
            .expect("another member's request opened the channel");
        assert!(!entry.wanted, "answering a request wants nothing here");
        // The pass that took the request in has run: nothing is owed.
        entry.dirty = false;
    }
    // The pass that took the request in: it answers, and settles nothing
    // of its own.
    member.service.pump(&member.endpoint, now, 1_000);
    member.service.queued = false;
    assert_eq!(
        member.service.next_pump_deadline(now),
        Some(now.checked_add(RECONCILE_INTERVAL).unwrap()),
        "a pass that answered only another member's request has no work of its own, only the \
         re-check of a channel whose directory announces nothing"
    );
    let woke = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&woke);
    let keys = ChannelKeyAccess::with_wake(
        member.service,
        Arc::new(move || flag.store(true, Ordering::Relaxed)),
    );
    assert!(matches!(
        keys.access(workspace_id, channel).0,
        ChannelAccess::Waiting
    ));
    assert!(
        woke.load(Ordering::Relaxed),
        "reading the channel wakes the pass that asks for it"
    );
}

/// #3998: devices that park between deadlines reach the same outcome
/// in the same simulated time as devices pumped on a fixed tick:
/// admission, then removal once the policy drops the member.
#[test]
fn parked_pumps_follow_the_policy_like_a_fixed_tick() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([1]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Unavailable
    ));
    let mut parked = [Some(now), Some(now)];
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Unavailable
    ));

    policy.allowed.lock().unwrap().insert(public(2));
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    parked[1] = Some(now); // what the endpoint's access does to its pump
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    let owner_epoch = ready_epoch(&owner.service.access(workspace_id, channel, now));
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, channel, now)),
        owner_epoch
    );
    assert!(
        parked[1].is_some(),
        "the admitted member re-checks the policy as every group member does"
    );

    // Ten idle seconds cost each group member one policy re-check a
    // reconcile interval.
    let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 10);
    assert!(idle <= 22, "{idle} passes in ten idle seconds");

    policy.allowed.lock().unwrap().remove(&public(2));
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Removed
    ));
}

/// #4022: with a directory that announces its changes, an idle group
/// has no deadline at all, and a revocation is enforced as soon as it is
/// announced, with no clock having run in between.
#[test]
fn an_announced_revocation_is_enforced_at_once_and_idle_groups_park() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    let mut parked = [Some(now), Some(now)];
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(ready_epoch(&laptop.service.access(workspace_id, channel, now)).is_some());
    assert_eq!(parked, [None, None], "both park once the member holds keys");

    // A minute with nothing announced runs no pass at all.
    let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 60);
    assert_eq!(idle, 0, "an idle group polls nothing");
    assert!(owner.service.waits_for_policy_change());

    // The revocation, announced. The owner is due at once, and one
    // pass removes the member.
    policy.allowed.lock().unwrap().remove(&public(2));
    epoch.fetch_add(1, Ordering::SeqCst);
    assert!(!owner.service.waits_for_policy_change());
    assert_eq!(owner.service.next_pump_deadline(now), Some(now));
    parked[0] = Some(now); // what the armed watch's wake does
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 1);
    assert!(matches!(
        laptop.service.access(workspace_id, channel, now),
        ChannelAccess::Removed
    ));
}

/// #4022: announcements in quick succession cost at most one re-check
/// a reconcile interval, and none waits longer than that.
#[test]
fn announcements_in_quick_succession_coalesce() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(1));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    let dir = tempfile::tempdir().unwrap();
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    owner.service.pump(&owner.endpoint, now, 1_000);
    let reconciled = now;
    now = now.checked_add(Duration::from_millis(100)).unwrap();
    epoch.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        owner.service.next_pump_deadline(now),
        Some(reconciled.checked_add(RECONCILE_INTERVAL).unwrap()),
        "a change right after a re-check waits out the interval"
    );
}

/// #4022: a grant that expires while nothing else happens is enforced
/// when it expires: the owner registers the expiry to be announced
/// at, parks with no timer of its own, and the announcement at the
/// expiry wakes the one pass that removes the member. The test thread's
/// wall clock is held, so the expiry is reached when the test moves it
/// there; no other test sees that time.
#[test]
fn a_grant_expiring_while_idle_is_enforced_at_its_expiry() {
    let start = tilo_clock::unix_now();
    let at = |second: u64| std::time::UNIX_EPOCH + Duration::from_secs(second);
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Process);
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    create(&mut owner, workspace_id, channel, Instant::now());
    let (owner_endpoint, laptop_endpoint) = (owner.endpoint, laptop.endpoint);
    let owner = ChannelKeyAccess::new(owner.service);
    let laptop = ChannelKeyAccess::new(laptop.service);
    // Match the drainer's Notify: concurrent announcements leave one
    // pending wake, not a queue of already-reconciled epochs to pump again.
    let (woke, wakes) = std::sync::mpsc::sync_channel(1);
    let watch = tilo_identity_host::policy_change::Watch::new(move || {
        let _ = woke.try_send(());
    });
    let parked = |owner: &ChannelKeyAccess| {
        tilo_test_env::wait_until("the owner parked", || {
            match owner.pump_until_due(&owner_endpoint) {
                Some(wait) => {
                    let _ = wakes.recv_timeout(wait);
                    false
                }
                None => owner.arm_policy_watch(&watch),
            }
        });
    };
    let expiry = start.saturating_add(3);

    // This thread pumps both devices, so the time it reads is the time
    // the owner judges the grant by.
    tilo_clock::testing::with_wall(at(start), || {
        assert!(ready_epoch(&owner.access(workspace_id, channel).0).is_some());
        tilo_test_env::wait_until("the laptop joined", || {
            let _ = owner.pump_until_due(&owner_endpoint);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            ready_epoch(&laptop.access(workspace_id, channel).0).is_some()
        });

        // Replace the admitted member's permission with an expiring
        // grant, announced through the production watch, and let the
        // owner register the expiry and park.
        policy.until.lock().unwrap().insert(public(2), expiry);
        policy.allowed.lock().unwrap().remove(&public(2));
        tilo_identity_host::policy_change::announce();
        parked(&owner);
        assert_eq!(
            policy.announce_at.lock().unwrap().last(),
            Some(&expiry),
            "the owner registered the expiry"
        );
        let _ = laptop.pump_until_due(&laptop_endpoint);
        assert!(ready_epoch(&laptop.access(workspace_id, channel).0).is_some());
    });

    // Short of the expiry, a pass (another announcement) keeps the member.
    tilo_clock::testing::with_wall(at(expiry - 1), || {
        tilo_identity_host::policy_change::announce();
        assert!(wakes.try_recv().is_ok(), "the armed watch woke");
        parked(&owner);
        let _ = laptop.pump_until_due(&laptop_endpoint);
        assert!(
            ready_epoch(&laptop.access(workspace_id, channel).0).is_some(),
            "removed before the grant ended"
        );
    });

    // The expiry: its announcement wakes the parked owner, whose
    // re-check (at most a reconcile interval after the last) removes the
    // member.
    tilo_clock::testing::with_wall(at(expiry), || {
        tilo_identity_host::policy_change::announce();
        assert!(wakes.try_recv().is_ok(), "the expiry woke the owner");
        tilo_test_env::wait_until("the member applied the removal", || {
            let _ = owner.pump_until_due(&owner_endpoint);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            matches!(
                laptop.access(workspace_id, channel).0,
                ChannelAccess::Removed
            )
        });
    });
}

/// #3964: an app endpoint waiting on its route's channel hears of its
/// admission and its removal the moment this device's service records
/// them, and of nothing that happens to another channel.
#[test]
fn a_route_hears_of_its_own_channel_changes_and_only_those() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let (channel, other) = (ChannelId::new(), ChannelId::new());
    let policy = Policy::readers([1]);
    let announced = Arc::new(std::sync::atomic::AtomicU64::new(0));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&announced)));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let Device {
        service, endpoint, ..
    } = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    let keys = ChannelKeyProvider::default();
    keys.set(Some(ChannelKeyAccess::new(service)));
    let laptop = keys.get().unwrap();
    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    assert!(matches!(
        laptop.access(workspace_id, channel).0,
        ChannelAccess::Unavailable
    ));
    let mut pump = |rounds: usize| {
        for _ in 0..rounds {
            now = now.checked_add(Duration::from_millis(250)).unwrap();
            owner.service.pump(&owner.endpoint, now, 1_000);
            laptop.pump_at(&endpoint, now);
        }
    };
    // A wait, polled by hand: parked on the service when this returns,
    // and woken (its waker called) only by a change it has to look at.
    let wait = |after: u64| {
        let keys = keys.clone();
        let mut waiting = Waiting::new(async move { keys.channel_changed(channel, after).await });
        assert_eq!(waiting.poll(), None, "nothing changed since {after}");
        waiting
    };

    let before = laptop.channel_change(channel);
    let mut waiting = wait(before);
    pump(8);
    assert_eq!(waiting.poll(), None, "not admitted, nothing changed");

    policy.allowed.lock().unwrap().insert(public(2));
    announced.fetch_add(1, Ordering::SeqCst);
    pump(8);
    assert!(waiting.woken(), "admission wakes the wait");
    let admitted = waiting.poll().expect("admission is announced");
    assert_ne!(admitted, before);
    assert!(matches!(
        laptop.access(workspace_id, channel).0,
        ChannelAccess::Waiting
    ));
    pump(8);
    assert!(ready_epoch(&laptop.access(workspace_id, channel).0).is_some());

    let mut waiting = wait(laptop.channel_change(channel));
    assert!(matches!(
        laptop.access(workspace_id, other).0,
        ChannelAccess::Waiting
    ));
    pump(20);
    assert!(ready_epoch(&laptop.access(workspace_id, other).0).is_some());
    assert_ne!(laptop.channel_change(other), 0, "the other channel changed");
    if let Some(change) = waiting.poll() {
        panic!("woken by another channel's change ({change})");
    }

    policy.allowed.lock().unwrap().remove(&public(2));
    announced.fetch_add(1, Ordering::SeqCst);
    pump(8);
    assert!(waiting.woken(), "removal wakes the wait");
    assert!(waiting.poll().is_some(), "removal is announced");
    assert!(matches!(
        laptop.access(workspace_id, channel).0,
        ChannelAccess::Removed
    ));
}

/// A future polled by hand, which counts the wakes it asks for.
struct Waiting {
    future: std::pin::Pin<Box<dyn std::future::Future<Output = u64>>>,
    wakes: Arc<WakeCount>,
}

#[derive(Default)]
struct WakeCount(std::sync::atomic::AtomicUsize);

impl std::task::Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Waiting {
    fn new(future: impl std::future::Future<Output = u64> + 'static) -> Self {
        Self {
            future: Box::pin(future),
            wakes: Arc::default(),
        }
    }

    /// Whether the future asked to be polled since it was last polled.
    fn woken(&self) -> bool {
        self.wakes.0.load(Ordering::SeqCst) > 0
    }

    fn poll(&mut self) -> Option<u64> {
        self.wakes.0.store(0, Ordering::SeqCst);
        let waker = std::task::Waker::from(Arc::clone(&self.wakes));
        match self
            .future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
        {
            std::task::Poll::Ready(change) => Some(change),
            std::task::Poll::Pending => None,
        }
    }
}

/// A device that is not a member yet (a browser, which learns nothing
/// about the workspace's governance but what the host carries) learns from
/// the workspace that the channel exists and is encrypted.
#[test]
fn a_new_channel_announces_its_genesis_to_the_workspace() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(5));
    let owner_dir = tempfile::tempdir().unwrap();
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
    let observer = Endpoint {
        workspace: Arc::clone(&workspace),
        id: 1,
        workspace_id,
    };
    workspace.queues.lock().unwrap().entry(1).or_default();
    assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
    run(&mut [&mut owner], &mut now, 3);
    let genesis: Vec<_> = observer
        .take_inbound()
        .into_iter()
        .filter_map(|inbound| match inbound {
            InboundChannelGovernance::Frame { frame, .. } => Some(frame),
            InboundChannelGovernance::AccessRequest { .. }
            | InboundChannelGovernance::Unfounded { .. } => None,
        })
        .collect();
    assert_eq!(genesis.len(), 1, "announced once per session");
    assert_eq!(genesis[0].channel, channel);
    assert_eq!(genesis[0].signed_by, public(5));
    assert!(matches!(
        genesis[0].action,
        collab_types::MlsGovernanceAction::Genesis
    ));
}

#[test]
fn delivery_survives_a_network_outage() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(4));
    policy.allowed.lock().unwrap().insert(public(3));
    let (owner_dir, phone_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 3, &policy, owner_dir.path());
    let mut phone = device(&workspace, workspace_id, 1, 4, &policy, phone_dir.path());
    create(&mut owner, workspace_id, channel, now);
    phone.service.access(workspace_id, channel, now);
    // The request gets through, then the network fails before the
    // genesis and the Welcome can. The phone, hearing of no group,
    // creates its own.
    run(&mut [&mut phone], &mut now, 1);
    workspace.down.store(true, Ordering::SeqCst);
    run(&mut [&mut owner, &mut phone], &mut now, 30);
    assert!(!owner.service.peers[&channel].peer.outbox().is_empty());
    workspace.down.store(false, Ordering::SeqCst);
    run(&mut [&mut owner, &mut phone], &mut now, 130);
    let epoch = ready_epoch(&phone.service.access(workspace_id, channel, now));
    assert!(epoch.is_some());
    assert_eq!(
        ready_epoch(&owner.service.access(workspace_id, channel, now)),
        epoch
    );
    assert_eq!(
        owner.service.peers[&channel].peer.members(),
        phone.service.peers[&channel].peer.members(),
        "one group"
    );
    assert!(
        owner.service.peers[&channel].peer.outbox().is_empty(),
        "the outbox drains once the transport takes it"
    );
}

#[test]
fn channel_state_and_gates_survive_a_restart() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(6));
    policy.allowed.lock().unwrap().insert(public(5));
    let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    {
        let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
        let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
        owner.service.access(workspace_id, channel, now);
        tablet.service.access(workspace_id, channel, now);
        run(&mut [&mut owner, &mut tablet], &mut now, 5);
        assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
    }
    for (id, seed, dir) in [(0, 5, owner_dir.path()), (1, 6, tablet_dir.path())] {
        let mut restarted = device(&workspace, workspace_id, id, seed, &policy, dir);
        assert!(
            refuses_plaintext(&restarted, channel),
            "the plaintext gate holds from the first op after a restart"
        );
        assert!(ready_epoch(&restarted.service.access(workspace_id, channel, now)).is_some());
    }
}

/// A device that stops before the join it took in was written (a page
/// closed as it entered) holds the state it asked with when it comes back:
/// it asks again and is admitted again. A join that was written stays.
#[test]
fn a_join_lost_before_it_was_written_is_asked_for_again() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(6));
    policy.allowed.lock().unwrap().insert(public(5));
    let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let step = |devices: &mut [&mut Device], now: &mut Instant| {
        *now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in devices.iter_mut() {
            device.service.pump(&device.endpoint, *now, 1_000);
        }
    };
    let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
    {
        let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
        owner.service.access(workspace_id, channel, now);
        tablet.service.access(workspace_id, channel, now);
        for _ in 0..40 {
            if ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some() {
                break;
            }
            step(&mut [&mut owner, &mut tablet], &mut now);
        }
        assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
        assert!(tablet.service.peers[&channel].peer.has_unsaved());
    }
    let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
    assert!(
        ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_none(),
        "the join was not written"
    );
    run(&mut [&mut owner, &mut tablet], &mut now, 120);
    assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
    // Written once it waited: a restart now keeps it.
    run(&mut [&mut owner, &mut tablet], &mut now, 3);
    assert!(!tablet.service.peers[&channel].peer.has_unsaved());
    drop(tablet);
    let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
    assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
}

/// A device writes under the keys a join brought before that join waits to
/// be written: the state travels with the write it sealed, so a page that
/// reloads just after writing keeps the admission it wrote under.
#[test]
fn a_write_under_a_fresh_join_leaves_its_state_written() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(6));
    policy.allowed.lock().unwrap().insert(public(5));
    let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let step = |devices: &mut [&mut Device], now: &mut Instant| {
        *now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in devices.iter_mut() {
            device.service.pump(&device.endpoint, *now, 1_000);
        }
    };
    let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
    {
        let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
        owner.service.access(workspace_id, channel, now);
        tablet.service.access(workspace_id, channel, now);
        for _ in 0..40 {
            if ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some() {
                break;
            }
            step(&mut [&mut owner, &mut tablet], &mut now);
        }
        assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
        assert!(tablet.service.peers[&channel].peer.has_unsaved());
        // A write sealed here leaves with the state written.
        tablet.service.save_pending();
        assert!(!tablet.service.peers[&channel].peer.has_unsaved());
    }
    let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
    assert!(
        ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some(),
        "the join the write went out with was written"
    );
}

/// Key loss: a laptop is lost with all its channel state. Restoring the
/// person's identity backup on a replacement machine brings back the
/// root identity, not the lost device key; onboarding certifies a new
/// device key. Once the workspace's directory names that certificate, the
/// policy allows the new device, a participant admits it, and its key
/// history opens everything written before the loss. The lost device is
/// removed, so it cannot read what is written afterwards.
#[test]
fn a_restored_identity_regains_the_whole_channel_on_its_new_device() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([7]);
    policy.allowed.lock().unwrap().insert(public(8));
    let (owner_dir, lost_dir, new_dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 7, &policy, owner_dir.path());
    let mut lost = device(&workspace, workspace_id, 1, 8, &policy, lost_dir.path());
    owner.service.access(workspace_id, channel, now);
    lost.service.access(workspace_id, channel, now);
    run(&mut [&mut owner, &mut lost], &mut now, 5);
    let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
        panic!("owner keys");
    };
    let before = keys
        .seal_content(
            artifact,
            collab_types::ContentRetention::Delta,
            b"before the loss",
        )
        .unwrap();
    let ChannelAccess::Ready(lost_keys) = lost.service.access(workspace_id, channel, now) else {
        panic!("the laptop was a member");
    };
    drop(lost);

    // The replacement machine's new device key replaces the lost one in
    // the person's certified device set.
    {
        let mut allowed = policy.allowed.lock().unwrap();
        allowed.remove(&public(8));
        allowed.insert(public(9));
    }
    let mut restored = device(&workspace, workspace_id, 2, 9, &policy, new_dir.path());
    assert!(matches!(
        restored.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut owner, &mut restored], &mut now, 5);
    let ChannelAccess::Ready(restored_keys) = restored.service.access(workspace_id, channel, now)
    else {
        panic!("the restored identity's new device is admitted");
    };
    assert!(
        restored_keys
            .open_content(artifact, &before.0, &before.1)
            .is_ok(),
        "its key history opens everything written before the loss"
    );
    let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
        panic!("owner keys");
    };
    let after = keys
        .seal_content(
            artifact,
            collab_types::ContentRetention::Delta,
            b"after the loss",
        )
        .unwrap();
    assert!(
        restored_keys
            .open_content(artifact, &after.0, &after.1)
            .is_ok()
    );
    assert!(
        lost_keys
            .open_content(artifact, &after.0, &after.1)
            .is_err(),
        "the lost device was removed and cannot read what follows"
    );
}

/// Copy `from` into `to`, as a disk copy does.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A machine copied from another re-keys its device. Its copied channel
/// state names the source's member, so it is dropped rather than used: the
/// branch takes no leaf of the source's. Once the person's new device is a
/// reader, a participant admits it, its key history opens what was written
/// before, and what either writes afterwards the other reads, while the
/// source stays the member it was.
#[test]
fn a_branched_device_rejoins_as_itself_beside_its_source() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([20]);
    let (source_dir, branch_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut source = device(&workspace, workspace_id, 0, 20, &policy, source_dir.path());
    let ChannelAccess::Ready(keys) = create(&mut source, workspace_id, channel, now) else {
        panic!("the source's keys");
    };
    let before = keys
        .seal_content(artifact, ContentRetention::Delta, b"before the copy")
        .unwrap();
    let source_epoch = keys.epoch();

    // The disk is copied; the copy re-keys to device 21 and keeps the
    // person's vault. Certified, it is the person's device and a reader.
    copy_tree(source_dir.path(), branch_dir.path());
    let copied = ChannelsDir::new(branch_dir.path().join("channels"))
        .workspace_dir(workspace_id)
        .join(format!("{}.peer", channel.0));
    assert!(copied.exists());
    policy.allowed.lock().unwrap().insert(public(21));
    let mut branch = sealed_device(
        &workspace,
        workspace_id,
        1,
        (20, 21),
        &policy,
        branch_dir.path(),
    );
    assert!(
        !copied.exists(),
        "the source's member state is dropped, never used"
    );
    assert!(matches!(
        branch.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut source, &mut branch], &mut now, 5);
    let ChannelAccess::Ready(branch_keys) = branch.service.access(workspace_id, channel, now)
    else {
        panic!("the branch is admitted as itself");
    };
    assert!(
        branch_keys
            .open_content(artifact, &before.0, &before.1)
            .is_ok(),
        "its key history opens what was written before the copy"
    );

    // Each reads what the other writes from now on.
    let ChannelAccess::Ready(source_keys) = source.service.access(workspace_id, channel, now)
    else {
        panic!("the source keeps its membership");
    };
    assert!(
        source_keys.epoch() > source_epoch,
        "admitting the branch is a commit"
    );
    let from_branch = branch_keys
        .seal_content(artifact, ContentRetention::Delta, b"from the branch")
        .unwrap();
    assert_eq!(
        source_keys
            .open_content(artifact, &from_branch.0, &from_branch.1)
            .unwrap()
            .as_slice(),
        b"from the branch"
    );
    let from_source = source_keys
        .seal_content(artifact, ContentRetention::Delta, b"from the source")
        .unwrap();
    assert_eq!(
        branch_keys
            .open_content(artifact, &from_source.0, &from_source.1)
            .unwrap()
            .as_slice(),
        b"from the source"
    );

    // Both come back as themselves after a restart.
    drop((source, branch));
    for (id, keys, dir) in [
        (0, (20, 20), source_dir.path()),
        (1, (20, 21), branch_dir.path()),
    ] {
        let mut restarted = sealed_device(&workspace, workspace_id, id, keys, &policy, dir);
        assert!(ready_epoch(&restarted.service.access(workspace_id, channel, now)).is_some());
    }
}

fn sealed_op(
    artifact: collab_types::ArtifactId,
    (metadata, payload): (EncryptionMetadata, Vec<u8>),
    kind: collab_types::OpKind,
) -> SignedOp {
    SignedOp::new_unsigned_with_encryption(
        payload,
        artifact,
        public(1),
        collab_types::AttestationId::default(),
        kind,
        std::time::SystemTime::UNIX_EPOCH,
        1,
        metadata,
    )
}

/// The key holder compacting a sealed record reads its whole history,
/// older epochs included, and seals the merged record as a current
/// checkpoint that opens as a new write; a new write still opens only
/// under the current epoch.
#[test]
fn a_key_holder_opens_history_and_a_current_checkpoint() {
    use collab_types::OpKind;
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(12));
    policy.allowed.lock().unwrap().insert(public(11));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 11, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 12, &policy, laptop_dir.path());
    let ChannelAccess::Ready(first) = create(&mut owner, workspace_id, channel, now) else {
        panic!("owner keys");
    };
    let old = sealed_op(
        artifact,
        first
            .seal_content(artifact, ContentRetention::Delta, b"before the rotation")
            .unwrap(),
        OpKind::Change,
    );
    laptop.service.access(workspace_id, channel, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    let keys = ChannelKeyAccess::new(owner.service);
    let (ChannelAccess::Ready(current), _) = keys.access(workspace_id, channel) else {
        panic!("owner keys");
    };
    assert!(current.epoch() > first.epoch(), "adding the laptop rotated");

    assert!(keys.open_new_write(workspace_id, &old).is_err());
    assert_eq!(
        keys.open_history_write(workspace_id, &old).unwrap(),
        b"before the rotation"
    );
    let sealed = current
        .seal_content(artifact, ContentRetention::Checkpoint, b"merged record")
        .unwrap();
    assert_eq!(sealed.0.epoch, current.epoch());
    assert_eq!(sealed.0.retention(), Some(ContentRetention::Checkpoint));
    let payload = collab_types::SealedCompaction::new(std::slice::from_ref(&old), sealed.1)
        .unwrap()
        .encode()
        .unwrap();
    let compaction = sealed_op(artifact, (sealed.0, payload), OpKind::Compaction);
    assert_eq!(
        keys.open_new_write(workspace_id, &compaction).unwrap(),
        b"merged record"
    );
    assert_eq!(
        keys.open_history_write(workspace_id, &compaction).unwrap(),
        b"merged record"
    );
}

fn sealed_write(artifact: collab_types::ArtifactId, keys: &ChannelContentKeys) -> SignedOp {
    let (metadata, payload) = keys
        .seal_content(artifact, collab_types::ContentRetention::Delta, b"the edit")
        .unwrap();
    SignedOp::new_unsigned_with_encryption(
        payload,
        artifact,
        public(10),
        collab_types::document_attestation_id("test.app", "workspace", ""),
        collab_types::OpKind::Change,
        std::time::SystemTime::now(),
        1,
        metadata,
    )
}

#[test]
fn a_renewal_keeps_a_current_ciphertext_and_reseals_one_from_before_a_rotation() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([10]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 10, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 11, &policy, laptop_dir.path());
    let ChannelAccess::Ready(before) = create(&mut owner, workspace_id, channel, now) else {
        panic!("the owner holds the channel's keys");
    };
    let stale = sealed_write(artifact, &before);
    policy.allowed.lock().unwrap().insert(public(11));
    laptop.service.access(workspace_id, channel, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    let access = ChannelKeyAccess::new(owner.service);
    let (ChannelAccess::Ready(current), _) = access.access(workspace_id, channel) else {
        panic!("the owner holds the channel's keys");
    };
    assert!(
        current.epoch() > before.epoch(),
        "adding a member rotated the epoch"
    );

    let fresh = sealed_write(artifact, &current);
    assert_eq!(
        access.renew_sealed(workspace_id, &fresh).unwrap(),
        (fresh.encryption.unwrap(), fresh.payload.clone()),
        "a current-epoch write renews with its identical ciphertext"
    );

    let (metadata, payload) = access.renew_sealed(workspace_id, &stale).unwrap();
    assert_eq!(metadata.epoch, current.epoch());
    assert_eq!(
        current
            .open_current_content(artifact, &metadata, &payload)
            .unwrap()
            .as_slice(),
        b"the edit",
        "the re-sealed content opens as a current write"
    );
    assert!(
        current
            .open_current_content(artifact, &stale.encryption.unwrap(), &stale.payload)
            .is_err(),
        "the stale ciphertext itself would be refused as a new write"
    );

    let mut tampered = stale;
    tampered.payload[0] ^= 1;
    assert!(access.renew_sealed(workspace_id, &tampered).is_err());
    let mut plaintext = fresh;
    plaintext.encryption = None;
    assert!(access.renew_sealed(workspace_id, &plaintext).is_err());
}

#[test]
fn a_device_without_ready_access_renews_nothing() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([12, 13]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 12, &policy, owner_dir.path());
    let laptop = device(&workspace, workspace_id, 1, 13, &policy, laptop_dir.path());
    let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
        panic!("the owner holds the channel's keys");
    };
    let write = sealed_write(artifact, &keys);
    let waiting = ChannelKeyAccess::new(laptop.service);
    let state = waiting.access(workspace_id, channel).0;
    assert!(matches!(state, ChannelAccess::Waiting), "{state:?}");
    assert!(
        waiting.renew_sealed(workspace_id, &write).is_err(),
        "a device still waiting for access renews nothing"
    );
}

#[test]
fn opening_history_never_creates_a_channel_without_an_existing_key() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let owner_dir = tempfile::tempdir().unwrap();
    let mut owner = device(
        &workspace,
        workspace_id,
        0,
        1,
        &Policy::readers([1, 2]),
        owner_dir.path(),
    );
    create(&mut owner, workspace_id, channel, Instant::now());
    let keys = ChannelKeyAccess::new(owner.service);
    let (ChannelAccess::Ready(held), _) = keys.access(workspace_id, channel) else {
        panic!("explicit fixture admission provides the original key");
    };
    let sealed = sealed_op(
        artifact,
        held.seal_content(artifact, ContentRetention::Delta, b"existing history")
            .unwrap(),
        collab_types::OpKind::Change,
    );
    let generation = keys.generation();
    assert_eq!(
        keys.open_history_write(workspace_id, &sealed).unwrap(),
        b"existing history"
    );
    assert_eq!(keys.generation(), generation);
    assert!(
        keys.open_history_write(WorkspaceId::new(), &sealed)
            .is_err()
    );
    assert_eq!(keys.generation(), generation);

    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = device(
        &workspace,
        workspace_id,
        0,
        1,
        &Policy::readers([1, 2]),
        fresh_dir.path(),
    );
    let fresh = ChannelKeyAccess::new(fresh.service);
    let generation = fresh.generation();
    assert!(fresh.open_history_write(workspace_id, &sealed).is_err());
    let state = fresh.inner.lock().unwrap();
    assert!(
        state.peers.is_empty(),
        "reading history must not mint replacement keys or request access"
    );
    assert!(
        !state.queued,
        "reading history must not enqueue channel governance"
    );
    assert_eq!(state.generation(), generation);
    drop(state);
}

include!("unopened_tests.rs");

/// A device whose secret store did not open keeps no channel state of
/// its own. A workspace is answered that it has no secret store; asking again
/// creates nothing, sends nothing and leaves its pump nothing to do, so
/// nobody asks on a timer.
#[test]
fn without_a_store_nothing_is_due() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    // The directory announces policy changes, as a session's does.
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
        std::sync::atomic::AtomicU64::new(1),
    )));
    let gates = Arc::new(Gates::default());
    let mut service = ChannelEncryption::without_store(
        Box::new(Directory {
            policy: policy.clone(),
        }),
        Arc::clone(&gates) as Arc<dyn ChannelGates>,
    );
    let endpoint = Endpoint::join(&workspace, workspace_id, 0);
    let now = Instant::now();
    assert!(matches!(
        service.access(workspace_id, channel, now),
        ChannelAccess::NoSecretStore
    ));
    let generation = service.generation();
    for later in 1..=3 {
        let now = now.checked_add(Duration::from_secs(later * 60)).unwrap();
        service.pump(&endpoint, now, 1_000);
        assert!(matches!(
            service.access(workspace_id, channel, now),
            ChannelAccess::NoSecretStore
        ));
        assert_eq!(service.next_pump_deadline(now), None, "nothing is due");
    }
    assert_eq!(service.generation(), generation, "nothing changed");
    assert!(service.peers.is_empty());
    assert!(service.existing_keys(workspace_id, channel).is_none());
    assert!(
        workspace.queues.lock().unwrap().values().all(Vec::is_empty),
        "no governance was sent"
    );
    // The workspace is a governed one too: still no state of its own.
    let mut governed = ChannelEncryption::without_store(Box::new(Directory { policy }), gates);
    assert!(matches!(
        governed.access(workspace_id, channel, now),
        ChannelAccess::NoSecretStore
    ));
    assert!(governed.peers.is_empty());
}

include!("personal_cluster_tests.rs");

include!("held_frame_tests.rs");

include!("lagging_policy_tests.rs");

include!("catch_up_tests.rs");

include!("turn_tests.rs");

/// A device key in custody that answers a signature only on a later poll, as a page's `WebCrypto`
/// key does.
struct LaterSigner(ed25519_dalek::SigningKey);

impl DeviceSigner for LaterSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        let mut answered = false;
        Box::pin(std::future::poll_fn(move |context| {
            if std::mem::replace(&mut answered, true) {
                std::task::Poll::Ready(Ok(signature))
            } else {
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }))
    }
}

/// A reader whose key signs later is admitted by a participant and opens what it seals; alone it
/// never founds a group, and it commits nothing when the policy moves.
#[test]
fn a_reader_with_custody_that_signs_later_reads_a_participants_channel() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([31, 32, 33]);
    let (desktop_dir, page_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let reader = |seed: u8, id: usize, dir: &Path| {
        workspace.queues.lock().unwrap().entry(id).or_default();
        Device {
            service: ChannelEncryption::open(
                ChannelEncryptionConfig {
                    state: Arc::new(ChannelsDir::new(dir.join("channels"))),
                    vault: VaultKey::new([seed; 32]),
                    signer: Arc::new(LaterSigner(key(seed))),
                    role: Role::Reader,
                },
                Box::new(Directory {
                    policy: policy.clone(),
                }),
                Arc::new(Gates::default()),
            )
            .unwrap(),
            gates: Arc::new(Gates::default()),
            endpoint: Endpoint::join(&workspace, workspace_id, id),
        }
    };
    let mut page = reader(32, 1, page_dir.path());

    // Alone, a reader waits: it founds no group, however long it waits.
    assert!(matches!(
        page.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut page], &mut now, 30);
    assert!(!page.service.peers[&channel].peer.knows_group());
    assert!(page.service.signing(), "its request waits on its custody");
    pollster::block_on(page.service.signatures());
    assert!(!page.service.signing());

    let mut desktop = device(&workspace, workspace_id, 0, 31, &policy, desktop_dir.path());
    let ChannelAccess::Ready(_) = create(&mut desktop, workspace_id, channel, now) else {
        panic!("the desktop founds the channel");
    };
    for _ in 0..10 {
        run(&mut [&mut desktop, &mut page], &mut now, 3);
        pollster::block_on(page.service.signatures());
    }
    let ChannelAccess::Ready(keys) = page.service.access(workspace_id, channel, now) else {
        panic!("the desktop admits the reader");
    };
    let ChannelAccess::Ready(desktop_keys) = desktop.service.access(workspace_id, channel, now)
    else {
        panic!("desktop keys");
    };
    assert_eq!(keys.epoch(), desktop_keys.epoch());
    let sealed = sealed_write(artifact, &desktop_keys);
    assert_eq!(
        ChannelKeyAccess::new(page.service)
            .open_new_write(workspace_id, &sealed)
            .unwrap(),
        b"the edit"
    );

    // The policy admits a third device that asks: the desktop commits it, never a reader.
    let mut page = reader(32, 1, page_dir.path());
    let third_dir = tempfile::tempdir().unwrap();
    let mut third = reader(33, 2, third_dir.path());
    third.service.access(workspace_id, channel, now);
    page.service.access(workspace_id, channel, now);
    for _ in 0..10 {
        run(&mut [&mut page, &mut third], &mut now, 3);
        pollster::block_on(third.service.signatures());
        assert!(page.service.peers[&channel].peer.outbox().is_empty());
    }
    assert!(matches!(
        third.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut desktop, &mut page, &mut third], &mut now, 10);
    pollster::block_on(third.service.signatures());
    run(&mut [&mut desktop, &mut page, &mut third], &mut now, 10);
    assert_eq!(
        ready_epoch(&third.service.access(workspace_id, channel, now)),
        ready_epoch(&page.service.access(workspace_id, channel, now)),
    );
}

/// What `device` heard the workspace publish, drained: how many access
/// requests reached it.
fn requests(device: &Device) -> usize {
    device
        .endpoint
        .take_inbound()
        .iter()
        .filter(|heard| matches!(heard, InboundChannelGovernance::AccessRequest { .. }))
        .count()
}

/// A device waiting for access whose request went unanswered asks again as
/// soon as the policy moves, rather than waiting out a backoff that has
/// doubled since: the workspace's governance arriving is what lets the
/// participants answer a request they could not decide before, and the
/// caller that waits on the keys gives up long before the backoff does.
#[test]
fn a_waiting_request_is_asked_again_when_the_policy_moves() {
    let workspace = Arc::new(Workspace::default());
    // A group the device is not in yet: its host answers nothing.
    workspace.silent.store(true, Ordering::SeqCst);
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([2]);
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    let (dir, listener_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut waiting = device(&workspace, workspace_id, 0, 2, &policy, dir.path());
    // Another endpoint hears what the device says; it takes no part in the
    // channel, so the request goes unanswered.
    let listener = device(&workspace, workspace_id, 1, 3, &policy, listener_dir.path());
    let mut now = Instant::now();
    assert!(matches!(
        waiting.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    waiting.service.pump(&waiting.endpoint, now, 1_000);
    let mut asked = requests(&listener);
    assert_eq!(asked, 1, "opening the channel asks");

    // Its own backoff holds the next ask back...
    now = now.checked_add(Duration::from_secs(1)).unwrap();
    waiting.service.pump(&waiting.endpoint, now, 1_001);
    asked += requests(&listener);
    assert_eq!(asked, 1, "the backoff holds the next ask back");

    // ...and a policy that moves asks again now, not when the backoff is due.
    epoch.fetch_add(1, Ordering::SeqCst);
    waiting.service.pump(&waiting.endpoint, now, 1_001);
    asked += requests(&listener);
    assert_eq!(asked, 2, "a moved policy asks again at once");
}

/// Two groups of one channel: the members of the one whose genesis lost
/// start over and join the winner, and every member reads what the others
/// seal from then on, and what was sealed on either side before.
#[test]
fn every_member_of_a_channel_that_forked_reads_the_winners_records() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([21, 22, 23]);
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut now = Instant::now();
    let mut alone = device(&workspace, workspace_id, 0, 21, &policy, dirs[0].path());
    let mut founder = device(&workspace, workspace_id, 1, 22, &policy, dirs[1].path());
    let mut joiner = device(&workspace, workspace_id, 2, 23, &policy, dirs[2].path());
    // One device founds a group it publishes to no one yet; two others found
    // and join another, and seal under it.
    let ChannelAccess::Ready(early) = create(&mut alone, workspace_id, channel, now) else {
        panic!("a founder holds its group's keys");
    };
    let before_alone = early
        .seal_content(artifact, ContentRetention::Delta, b"sealed alone")
        .unwrap();
    create(&mut founder, workspace_id, channel, now);
    joiner.service.access(workspace_id, channel, now);
    run(&mut [&mut founder, &mut joiner], &mut now, 10);
    let ChannelAccess::Ready(pair) = joiner.service.access(workspace_id, channel, now) else {
        panic!("the joiner is in the founder's group");
    };
    let before_pair = pair
        .seal_content(artifact, ContentRetention::Delta, b"sealed by the pair")
        .unwrap();

    // The two groups meet.
    let started = now;
    let mut devices = [alone, founder, joiner];
    let mut converged = None;
    for _ in 0..240 {
        {
            let [a, b, c] = &mut devices;
            run(&mut [a, b, c], &mut now, 1);
        }
        let epochs: Vec<_> = devices
            .iter_mut()
            .map(|device| ready_epoch(&device.service.access(workspace_id, channel, now)))
            .collect();
        let members: Vec<_> = devices
            .iter()
            .map(|device| device.service.peers[&channel].peer.members())
            .collect();
        if epochs.iter().all(Option::is_some)
            && epochs.windows(2).all(|pair| pair[0] == pair[1])
            && members.windows(2).all(|pair| pair[0] == pair[1])
            && members[0].len() == 3
        {
            converged = Some(now.duration_since(started));
            break;
        }
    }
    let converged = converged.expect("every member ends up in one group");
    eprintln!("a forked channel converged in {converged:?} of simulated time");

    let keys: Vec<ChannelContentKeys> = devices
        .iter_mut()
        .map(
            |device| match device.service.access(workspace_id, channel, now) {
                ChannelAccess::Ready(keys) => keys,
                other => panic!("every member reads the channel: {other:?}"),
            },
        )
        .collect();
    for (at, writer) in keys.iter().enumerate() {
        let sealed = writer
            .seal_content(artifact, ContentRetention::Delta, b"after the fork")
            .unwrap();
        for reader in &keys {
            assert_eq!(
                reader
                    .open_content(artifact, &sealed.0, &sealed.1)
                    .unwrap()
                    .as_slice(),
                b"after the fork",
                "member {at}'s write reads at every member"
            );
        }
    }
    // A commit hands the keys a member kept from the branch it left; give the
    // last one time to land, then every member reads both sides' history.
    {
        let [a, b, c] = &mut devices;
        run(&mut [a, b, c], &mut now, 10);
    }
    for device in &mut devices {
        let ChannelAccess::Ready(keys) = device.service.access(workspace_id, channel, now) else {
            panic!("every member still reads the channel");
        };
        for (sealed, text) in [
            (&before_alone, b"sealed alone".as_slice()),
            (&before_pair, b"sealed by the pair".as_slice()),
        ] {
            assert_eq!(
                keys.open_content(artifact, &sealed.0, &sealed.1)
                    .unwrap()
                    .as_slice(),
                text,
                "what either side sealed before the groups met reads everywhere"
            );
        }
    }
}

/// A waiting device admitted with a member key it has since replaced asks
/// again with the one it holds as soon as that admission reaches it, rather
/// than on its backoff or when it next mints a fresh key.
#[test]
fn a_device_admitted_with_a_key_it_replaced_asks_again_at_once() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([31, 32]);
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut now = Instant::now();
    let mut founder = device(&workspace, workspace_id, 0, 31, &policy, dirs[0].path());
    let mut reader = device(&workspace, workspace_id, 1, 32, &policy, dirs[1].path());
    create(&mut founder, workspace_id, channel, now);
    run(&mut [&mut founder], &mut now, 1);
    // The reader hears the group and asks to join; the founder is away.
    reader.service.access(workspace_id, channel, now);
    run(&mut [&mut reader], &mut now, 1);
    // Nothing the reader says reaches anyone for a while: it asks again
    // with a fresh member key once its first one went unanswered.
    workspace.down.store(true, Ordering::SeqCst);
    run(&mut [&mut reader], &mut now, 65);
    workspace.down.store(false, Ordering::SeqCst);

    // The founder comes back and admits the reader with its first key.
    let back = now;
    for _ in 0..90 {
        run(&mut [&mut founder, &mut reader], &mut now, 1);
        if ready_epoch(&reader.service.access(workspace_id, channel, now)).is_some() {
            break;
        }
    }
    let joined = now.duration_since(back);
    assert!(
        ready_epoch(&reader.service.access(workspace_id, channel, now)).is_some(),
        "the reader joins"
    );
    assert!(
        joined <= Duration::from_secs(3),
        "the reader asks again as soon as the stale admission reaches it, joined after {joined:?}"
    );
}

/// A device that cannot name a channel's record keeps a genesis competing
/// with its group's until it can, and then moves to the winner's group.
#[test]
fn a_competing_genesis_is_judged_once_the_device_names_the_record() {
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([41, 42, 43]);
    let reader_policy = Policy::readers([41, 42, 43]);
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut now = Instant::now();
    // Two devices found a group each, heard by a listener and by the reader's
    // endpoint, should the reader join that group.
    let founded = |seed: u8, dir: &Path| {
        let workspace = Arc::new(Workspace::default());
        let listener = Endpoint::join(&workspace, workspace_id, 9);
        Endpoint::join(&workspace, workspace_id, 1);
        let mut founder = device(&workspace, workspace_id, 0, seed, &policy, dir);
        create(&mut founder, workspace_id, channel, now);
        run(&mut [&mut founder], &mut now.clone(), 1);
        let genesis = listener
            .take_inbound()
            .into_iter()
            .find_map(|inbound| match inbound {
                InboundChannelGovernance::Frame { frame, .. } => Some(*frame),
                InboundChannelGovernance::AccessRequest { .. }
                | InboundChannelGovernance::Unfounded { .. } => None,
            })
            .unwrap();
        (workspace, founder, genesis)
    };
    let first = founded(41, dirs[0].path());
    let second = founded(42, dirs[1].path());
    let ((workspace, mut loser, _), (_, _, winning)) =
        if first.2.digest().unwrap() < second.2.digest().unwrap() {
            (second, first)
        } else {
            (first, second)
        };
    // The reader hears the genesis that loses, and joins its group.
    let mut reader = device(
        &workspace,
        workspace_id,
        1,
        43,
        &reader_policy,
        dirs[2].path(),
    );
    reader.service.access(workspace_id, channel, now);
    run(&mut [&mut loser, &mut reader], &mut now, 10);
    let epoch = ready_epoch(&reader.service.access(workspace_id, channel, now));
    assert!(epoch.is_some(), "the reader joins the loser's group");

    // The winner's genesis reaches the reader while it cannot name the record.
    reader_policy.unnamed.lock().unwrap().insert(channel);
    workspace
        .queues
        .lock()
        .unwrap()
        .entry(1)
        .or_default()
        .push(InboundChannelGovernance::Frame {
            workspace: workspace_id,
            frame: Box::new(winning),
        });
    run(&mut [&mut reader], &mut now, 2);
    assert_eq!(
        ready_epoch(&reader.service.access(workspace_id, channel, now)),
        epoch,
        "a genesis the reader cannot judge does not move it"
    );

    // Named again, the winner's genesis decides: the reader starts over.
    reader_policy.unnamed.lock().unwrap().remove(&channel);
    run(&mut [&mut reader], &mut now, 1);
    assert!(
        reader.service.peers[&channel].peer.access() == Access::Waiting,
        "the reader leaves the group whose genesis lost"
    );
}

/// A member whose link to the device it reaches the workspace through was down
/// when a channel's group changed heard nothing of the commit, which that
/// device published once. What the device hands a member that links
/// ([`ChannelEncryption::standing_governance`]) makes it current again: nobody
/// has to commit a second time for it to leave the epoch it was left at.
#[test]
fn a_member_that_links_after_a_commit_is_handed_the_channels_head() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::default();
    for seed in [3, 4, 5] {
        policy.allowed.lock().unwrap().insert(public(seed));
    }
    let (owner_dir, phone_dir, tablet_dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 3, &policy, owner_dir.path());
    let mut phone = device(&workspace, workspace_id, 1, 4, &policy, phone_dir.path());
    create(&mut owner, workspace_id, channel, now);
    phone.service.access(workspace_id, channel, now);
    run(&mut [&mut owner, &mut phone], &mut now, 30);
    let joined = ready_epoch(&phone.service.access(workspace_id, channel, now));
    assert!(joined.is_some(), "the phone is in the channel");
    assert_eq!(
        ready_epoch(&owner.service.access(workspace_id, channel, now)),
        joined
    );

    // The tablet is admitted while nothing reaches the phone: what the others
    // publish meanwhile is lost to it, as on a link that is down.
    let mut tablet = device(&workspace, workspace_id, 2, 5, &policy, tablet_dir.path());
    tablet.service.access(workspace_id, channel, now);
    for _ in 0..240 {
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        owner.service.pump(&owner.endpoint, now, 1_000);
        tablet.service.pump(&tablet.endpoint, now, 1_000);
        workspace
            .queues
            .lock()
            .unwrap()
            .entry(phone.endpoint.id)
            .or_default()
            .clear();
    }
    let ahead = ready_epoch(&owner.service.access(workspace_id, channel, now));
    assert_eq!(
        ready_epoch(&tablet.service.access(workspace_id, channel, now)),
        ahead,
        "the tablet is in the channel"
    );
    assert!(
        joined < ahead,
        "the channel moved on: {joined:?} then {ahead:?}"
    );
    assert_eq!(
        ready_epoch(&phone.service.access(workspace_id, channel, now)),
        joined,
        "the phone heard nothing of it"
    );

    // The phone's link is up again, and the device it reaches the workspace
    // through hands it what its channels hold.
    let handed = owner.service.standing_governance(workspace_id);
    assert!(
        handed.iter().all(|message| matches!(
            message,
            NeighbourMsg::MlsGovernance { workspace, .. } if *workspace == workspace_id.to_string()
        )),
        "frames of the workspace's channels, and nothing else"
    );
    assert!(
        owner
            .service
            .standing_governance(WorkspaceId::new())
            .is_empty(),
        "and none of another workspace's"
    );
    workspace
        .queues
        .lock()
        .unwrap()
        .entry(phone.endpoint.id)
        .or_default()
        .extend(
            handed
                .into_iter()
                .filter_map(InboundChannelGovernance::from_message),
        );
    run(&mut [&mut phone], &mut now, 2);
    assert_ne!(
        ready_epoch(&phone.service.access(workspace_id, channel, now)),
        joined,
        "the phone no longer seals under the epoch it was left at"
    );
    run(&mut [&mut owner, &mut phone, &mut tablet], &mut now, 130);
    let settled = ready_epoch(&owner.service.access(workspace_id, channel, now));
    assert!(settled.is_some());
    assert_eq!(
        ready_epoch(&phone.service.access(workspace_id, channel, now)),
        settled
    );
    assert_eq!(
        owner.service.peers[&channel].peer.members(),
        phone.service.peers[&channel].peer.members(),
        "one group"
    );
}
