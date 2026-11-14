use super::*;
use crate::gates::Gates;
use crate::testing::*;
use collab_types::{ArtifactId, ContentRetention};
use std::path::Path;
use std::sync::atomic::Ordering;

struct Device {
    /// Whether this device founds the workspace's group: the one that
    /// created it ([`create`]).
    founds: Arc<std::sync::atomic::AtomicBool>,
    service: WorkspaceKeys,
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
    // The first device created the workspace.
    let founds = Arc::new(std::sync::atomic::AtomicBool::new(id == 0));
    let service = WorkspaceKeys::open(
        KeysConfig {
            state: Arc::new(KeyDir::new(dir.join("groups"))),
            vault: VaultKey::new([vault; 32]),
            signer: std::sync::Arc::new(key(seed)),
            role: crate::KeyRole::Committer,
        },
        Box::new(Directory {
            policy: policy.clone(),
            founds: Arc::clone(&founds),
        }),
        Arc::clone(&gates) as Arc<dyn GateRegistry>,
    )
    .unwrap();
    Device {
        founds,
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

/// `device` creates the workspace and founds its group.
fn create(device: &mut Device, workspace_id: WorkspaceId, now: Instant) -> KeyAccess {
    device.founds.store(true, Ordering::SeqCst);
    device.service.access(workspace_id, now);
    device.service.found(workspace_id, now, 1_000);
    device.service.access(workspace_id, now)
}

fn ready_epoch(access: &KeyAccess) -> Option<u64> {
    match access {
        KeyAccess::Ready(keys) => Some(keys.epoch()),
        _ => None,
    }
}

fn refuses_plaintext(device: &Device) -> bool {
    !device
        .gates
        .allows_document(&device.endpoint.workspace_id.to_string(), None)
}

#[test]
fn observing_live_keys_never_opens_a_group_or_rejoins_after_removal() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
    for service in [&owner.service, &member.service] {
        let generation = service.generation();
        assert!(service.existing_keys(workspace_id).is_none());
        assert!(service.peers.is_empty());
        assert_eq!(service.generation(), generation);
    }

    // Only this explicit fixture admission creates or requests keys.
    let mut now = Instant::now();
    owner.service.access(workspace_id, now);
    member.service.access(workspace_id, now);
    run(&mut [&mut owner, &mut member], &mut now, 5);
    assert!(member.service.existing_keys(workspace_id).is_some());
    assert!(member.service.existing_keys(WorkspaceId::new()).is_none());
    assert!(owner.service.existing_keys(WorkspaceId::new()).is_none());

    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut member], &mut now, 3);
    let generation = member.service.generation();
    let rejoin = member.service.peers.get(&workspace_id).unwrap().rejoin;
    assert!(!rejoin);
    assert!(member.service.existing_keys(workspace_id).is_none());
    assert_eq!(member.service.generation(), generation);
    assert!(!member.service.peers.get(&workspace_id).unwrap().rejoin);
}

#[test]
fn opening_a_group_wakes_a_parked_pump_and_reading_it_does_not() {
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
    let access = SharedKeys::with_wake(
        owner.service,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    access.access(workspace_id);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "a new group has a genesis to announce"
    );
    access.access(workspace_id);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "an open group queues nothing new"
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
    let member = SharedKeys::with_wake(
        member.service,
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert!(matches!(member.access(workspace_id).0, KeyAccess::Waiting));
    assert_eq!(
        member_wakes.load(Ordering::SeqCst),
        1,
        "a new member has an access request to make"
    );
    member.access(workspace_id);
    assert_eq!(member_wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_new_group_is_encrypted_from_creation_and_follows_the_policy() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
    assert!(refuses_plaintext(&owner), "encrypted from creation");
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Unavailable
    ));

    // Not named yet: nobody can admit it, and its route hears when that
    // changes.
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Unavailable
    ));

    policy.allowed.lock().unwrap().insert(public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let owner_epoch = ready_epoch(&owner.service.access(workspace_id, now));
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, now)),
        owner_epoch
    );
    assert!(
        refuses_plaintext(&laptop),
        "gate: {:?} peer: {:?}",
        laptop.service.registered.get(&workspace_id),
        laptop
            .service
            .peers
            .get(&workspace_id)
            .map(|e| (e.peer.access(), e.peer.gate()))
    );

    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Removed
    ));
}

/// Every record of a workspace seals under the workspace's one group: a
/// device admitted later opens what each record held before it joined, and
/// once removed it opens nothing sealed after.
#[test]
fn a_later_member_reads_old_records_and_a_removed_device_no_new_ones() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1]);
    let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let KeyAccess::Ready(keys) = create(&mut owner, workspace_id, now) else {
        panic!("the owner holds the workspace's keys");
    };
    let old = [
        collab_types::document_artifact_id("so.tilo.notes", &workspace_id.to_string(), "minutes"),
        collab_types::document_artifact_id("so.tilo.sheets", &workspace_id.to_string(), "payroll"),
    ]
    .map(|artifact| {
        let sealed = keys
            .seal_content(artifact, ContentRetention::Delta, b"before")
            .unwrap();
        (artifact, sealed)
    });

    policy.allowed.lock().unwrap().insert(public(2));
    laptop.service.access(workspace_id, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 16);
    let KeyAccess::Ready(held) = laptop.service.access(workspace_id, now) else {
        panic!("the laptop holds the workspace's keys");
    };
    for (artifact, (metadata, payload)) in &old {
        assert!(
            metadata.epoch < held.epoch(),
            "sealed before the laptop joined"
        );
        let opened = held.open_content(*artifact, metadata, payload).unwrap();
        assert_eq!(opened.as_slice(), b"before");
    }
    // Content stays bound to its record.
    let (_, (metadata, payload)) = &old[0];
    assert!(held.open_content(old[1].0, metadata, payload).is_err());

    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Removed
    ));
    let artifact = ArtifactId::new();
    let KeyAccess::Ready(keys) = owner.service.access(workspace_id, now) else {
        panic!("the owner holds the workspace's keys");
    };
    let (metadata, payload) = keys
        .seal_content(artifact, ContentRetention::Delta, b"after")
        .unwrap();
    assert!(metadata.epoch > held.epoch());
    assert!(held.open_content(artifact, &metadata, &payload).is_err());
}

/// A participant that cannot tell who holds the workspace's keys (its
/// governance is unknown here) removes no one. Once it can tell again, the
/// group follows the policy once more.
#[test]
fn a_participant_that_cannot_tell_who_holds_the_keys_removes_no_one() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    for device in [&mut owner, &mut laptop] {
        device.service.access(workspace_id, now);
    }
    run(&mut [&mut owner, &mut laptop], &mut now, 8);
    let epoch = ready_epoch(&laptop.service.access(workspace_id, now));
    assert!(epoch.is_some());
    assert_eq!(ready_epoch(&owner.service.access(workspace_id, now)), epoch);

    // The governance is no longer known.
    policy.unknown.store(true, Ordering::SeqCst);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    assert_eq!(ready_epoch(&owner.service.access(workspace_id, now)), epoch);
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, now)),
        epoch
    );

    // Known again, the group follows the policy: the laptop is removed.
    policy.unknown.store(false, Ordering::SeqCst);
    policy.allowed.lock().unwrap().remove(&public(2));
    run(&mut [&mut owner, &mut laptop], &mut now, 3);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Removed
    ));
    assert!(ready_epoch(&owner.service.access(workspace_id, now)) > epoch);
}
/// The access request device `seed` makes for `group`, with its state
/// under `dir`.
fn member_request(dir: &Path, workspace: WorkspaceId, seed: u8) -> MlsAccessRequestV1 {
    std::fs::create_dir_all(dir).unwrap();
    GroupPeer::waiting(
        Some(std::sync::Arc::new(key_group::Files::new(dir))),
        workspace,
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
/// [`WorkspaceKeys::access`] wakes the device as the installed pump
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

/// #3998: a device in a group's group re-checks its policy once a
/// reconcile interval, whoever created the group; a device with no
/// group has nothing to pump.
#[test]
fn only_a_group_member_or_an_unfinished_member_has_a_pump_deadline() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(2));
    let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
    // The member, the one device that reads the workspace, created it.
    member.founds.store(true, Ordering::SeqCst);
    assert_eq!(owner.service.next_pump_deadline(now), None, "no group");
    assert_eq!(member.service.next_pump_deadline(now), None, "no group");

    owner.service.access(workspace_id, now);
    member.service.access(workspace_id, now);
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
    assert!(ready_epoch(&member.service.access(workspace_id, now)).is_some());
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
    let dir = tempfile::tempdir().unwrap();
    // The directory announces its policy changes, so nothing but the
    // request backoff is ever due here.
    let policy = Policy::readers([1, 2]);
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
        std::sync::atomic::AtomicU64::new(1),
    )));
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
    // The group's group exists: this device saw its genesis, so it
    // waits to be admitted instead of founding one of its own.
    member.service.groups_seen.insert(workspace_id);
    let now = Instant::now();
    assert!(matches!(
        member.service.access(workspace_id, now),
        KeyAccess::Waiting
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
/// and asks nothing itself. The first read that wants the group asks
/// for it: the read queues the pass, so a parked pump sends the request
/// instead of sleeping with the keys never asked for.
#[test]
fn a_read_that_first_wants_a_group_queues_the_pass_that_asks() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2]);
    let dir = tempfile::tempdir().unwrap();
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
    // The group exists; this device only learns of it on admission.
    member.service.groups_seen.insert(workspace_id);
    let now = Instant::now();
    let request = member_request(&dir.path().join("asking"), workspace_id, 1);
    member
        .service
        .receive_request(workspace_id, request, now, 1_000);
    {
        let entry = member
            .service
            .peers
            .get_mut(&workspace_id)
            .expect("another member's request opened the group");
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
         re-check of a workspace_id whose directory announces nothing"
    );
    let woke = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&woke);
    let keys = SharedKeys::with_wake(
        member.service,
        Arc::new(move || flag.store(true, Ordering::Relaxed)),
    );
    assert!(matches!(keys.access(workspace_id).0, KeyAccess::Waiting));
    assert!(
        woke.load(Ordering::Relaxed),
        "reading the group wakes the pass that asks for it"
    );
}

/// #3998: devices that park between deadlines reach the same outcome
/// in the same simulated time as devices pumped on a fixed tick:
/// admission, then removal once the policy drops the member.
#[test]
fn parked_pumps_follow_the_policy_like_a_fixed_tick() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Unavailable
    ));
    let mut parked = [Some(now), Some(now)];
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Unavailable
    ));

    policy.allowed.lock().unwrap().insert(public(2));
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    parked[1] = Some(now); // what the endpoint's access does to its pump
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    let owner_epoch = ready_epoch(&owner.service.access(workspace_id, now));
    assert_eq!(
        ready_epoch(&laptop.service.access(workspace_id, now)),
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
        laptop.service.access(workspace_id, now),
        KeyAccess::Removed
    ));
}

/// #4022: with a directory that announces its changes, an idle group
/// has no deadline at all, and a revocation is enforced as soon as it is
/// announced, with no clock having run in between.
#[test]
fn an_announced_revocation_is_enforced_at_once_and_idle_groups_park() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
    assert!(matches!(
        laptop.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    let mut parked = [Some(now), Some(now)];
    run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
    assert!(ready_epoch(&laptop.service.access(workspace_id, now)).is_some());
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
        laptop.service.access(workspace_id, now),
        KeyAccess::Removed
    ));
}

/// #4022: announcements in quick succession cost at most one re-check
/// a reconcile interval, and none waits longer than that.
#[test]
fn announcements_in_quick_succession_coalesce() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(1));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    let dir = tempfile::tempdir().unwrap();
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
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
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Process);
    policy.allowed.lock().unwrap().insert(public(2));
    policy.allowed.lock().unwrap().insert(public(1));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    create(&mut owner, workspace_id, Instant::now());
    let (owner_endpoint, laptop_endpoint) = (owner.endpoint, laptop.endpoint);
    let owner = SharedKeys::new(owner.service);
    let laptop = SharedKeys::new(laptop.service);
    // Match the drainer's Notify: concurrent announcements leave one
    // pending wake, not a queue of already-reconciled epochs to pump again.
    let (woke, wakes) = std::sync::mpsc::sync_channel(1);
    let watch = tilo_identity_host::policy_change::Watch::new(move || {
        let _ = woke.try_send(());
    });
    let parked = |owner: &SharedKeys| {
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
        assert!(ready_epoch(&owner.access(workspace_id).0).is_some());
        tilo_test_env::wait_until("the laptop joined", || {
            let _ = owner.pump_until_due(&owner_endpoint);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            ready_epoch(&laptop.access(workspace_id).0).is_some()
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
        assert!(ready_epoch(&laptop.access(workspace_id).0).is_some());
    });

    // Short of the expiry, a pass (another announcement) keeps the member.
    tilo_clock::testing::with_wall(at(expiry - 1), || {
        tilo_identity_host::policy_change::announce();
        assert!(wakes.try_recv().is_ok(), "the armed watch woke");
        parked(&owner);
        let _ = laptop.pump_until_due(&laptop_endpoint);
        assert!(
            ready_epoch(&laptop.access(workspace_id).0).is_some(),
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
            matches!(laptop.access(workspace_id).0, KeyAccess::Removed)
        });
    });
}

/// An app endpoint waiting on its workspace's keys hears of its admission
/// and its removal the moment this device's service records them.
#[test]
fn an_endpoint_hears_of_its_admission_and_removal() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1]);
    let announced = Arc::new(std::sync::atomic::AtomicU64::new(0));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&announced)));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
    let Device {
        service, endpoint, ..
    } = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
    let keys = KeyProvider::default();
    keys.set(Some(SharedKeys::new(service)));
    let laptop = keys.get().unwrap();
    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
    assert!(matches!(
        laptop.access(workspace_id).0,
        KeyAccess::Unavailable
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
        let mut waiting =
            Waiting::new(async move { keys.workspace_changed(workspace_id, after).await });
        assert_eq!(waiting.poll(), None, "nothing changed since {after}");
        waiting
    };

    let before = laptop.workspace_change(workspace_id);
    let mut waiting = wait(before);
    pump(8);
    assert_eq!(waiting.poll(), None, "not admitted, nothing changed");

    policy.allowed.lock().unwrap().insert(public(2));
    announced.fetch_add(1, Ordering::SeqCst);
    pump(8);
    assert!(waiting.woken(), "admission wakes the wait");
    let admitted = waiting.poll().expect("admission is announced");
    assert_ne!(admitted, before);
    assert!(matches!(laptop.access(workspace_id).0, KeyAccess::Waiting));
    pump(8);
    assert!(ready_epoch(&laptop.access(workspace_id).0).is_some());

    let mut waiting = wait(laptop.workspace_change(workspace_id));
    policy.allowed.lock().unwrap().remove(&public(2));
    announced.fetch_add(1, Ordering::SeqCst);
    pump(8);
    assert!(waiting.woken(), "removal wakes the wait");
    assert!(waiting.poll().is_some(), "removal is announced");
    assert!(matches!(laptop.access(workspace_id).0, KeyAccess::Removed));
}

/// A request for a workspace's keys waits for its group without holding the
/// service: the identity host's ask and a read of the keys this device holds
/// answer at once meanwhile, and the wait ends with the group.
#[test]
fn a_request_waiting_for_keys_leaves_the_other_asks_answered() {
    use runtime_identity::wire::KeyAccess as Wire;
    use tilo_identity_host::sources::GroupKeySource as _;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let workspace = Arc::new(Workspace::default());
    let held = WorkspaceId::new();
    let awaited = WorkspaceId::new();
    let policy = Policy::readers([1, 2]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, held, 0, 1, &policy, owner_dir.path());
    let laptop = device(&workspace, held, 1, 2, &policy, laptop_dir.path());
    let keys = KeyProvider::default();
    keys.set(Some(SharedKeys::new(laptop.service)));
    let laptop_at = |workspace_id| Endpoint::join(&workspace, workspace_id, 1);
    let held_at = laptop_at(held);
    let awaited_at = laptop_at(awaited);

    // The laptop holds `held`'s keys, and has only asked for `awaited`'s.
    assert!(ready_epoch(&create(&mut owner, held, now)).is_some());
    tilo_test_env::wait_until("the laptop holds the workspace's keys", || {
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        owner.service.pump(&owner.endpoint, now, 1_000);
        let laptop = keys.get().unwrap();
        laptop.pump_at(&held_at, now);
        ready_epoch(&laptop.access(held).0).is_some()
    });
    assert!(matches!(
        keys.get().unwrap().access(awaited).0,
        KeyAccess::Waiting
    ));

    // The host asks for `awaited`'s keys, waiting for its group.
    let _entered = runtime.enter();
    let asking = keys.clone();
    let workspace_name = awaited.to_string();
    let mut waiting = Waiting::new(async move {
        asking
            .group_keys(&workspace_name, true, Duration::from_secs(30))
            .await
    });
    assert!(
        waiting.poll().is_none(),
        "a request for keys waits for the group"
    );

    // The service answers the other asks while that one waits: the identity
    // host's for the same workspace, and a read of the keys this device holds.
    let (answered, replies) = std::sync::mpsc::channel();
    let answering = keys.clone();
    let asked = std::thread::spawn(move || {
        let ready = answering.access(held, 0);
        let still_waiting = answering.access(awaited, 0);
        let stored = answering.get().unwrap().record_keys(held);
        let _ = answered.send((ready, still_waiting, stored));
    });
    let (ready, still_waiting, stored) = replies
        .recv_timeout(Duration::from_secs(5))
        .expect("the service answers while a request waits for keys");
    asked.join().unwrap();
    assert!(matches!(ready, Some(Ok((Wire::Ready, _, _)))), "{ready:?}");
    assert!(
        matches!(still_waiting, Some(Ok((Wire::Waiting, _, _)))),
        "{still_waiting:?}"
    );
    assert!(matches!(stored, Some(Ok(_))), "{stored:?}");
    assert!(
        waiting.poll().is_none(),
        "the waiting request has not run: its group is not there"
    );

    // Its group arrives, and the request answers with its keys.
    assert!(ready_epoch(&create(&mut owner, awaited, now)).is_some());
    let owner_at = Endpoint::join(&workspace, awaited, 0);
    for _ in 0..40 {
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        owner.service.pump(&owner_at, now, 1_000);
        keys.get().unwrap().pump_at(&awaited_at, now);
    }
    assert!(
        matches!(waiting.poll(), Some(Some(Ok(_)))),
        "the request answers once its group's keys arrive"
    );
}

/// A future polled by hand, which counts the wakes it asks for.
struct Waiting<T> {
    future: std::pin::Pin<Box<dyn Future<Output = T>>>,
    wakes: Arc<WakeCount>,
}

#[derive(Default)]
struct WakeCount(std::sync::atomic::AtomicUsize);

impl std::task::Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl<T> Waiting<T> {
    fn new(future: impl Future<Output = T> + 'static) -> Self {
        Self {
            future: Box::pin(future),
            wakes: Arc::default(),
        }
    }

    /// Whether the future asked to be polled since it was last polled.
    fn woken(&self) -> bool {
        self.wakes.0.load(Ordering::SeqCst) > 0
    }

    fn poll(&mut self) -> Option<T> {
        self.wakes.0.store(0, Ordering::SeqCst);
        let waker = std::task::Waker::from(Arc::clone(&self.wakes));
        match self
            .future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
        {
            std::task::Poll::Ready(settled) => Some(settled),
            std::task::Poll::Pending => None,
        }
    }
}

/// A device that is not a member yet (a browser, which learns nothing
/// about the workspace's governance but what the host carries) learns from
/// the workspace that the group exists and is encrypted.
#[test]
fn a_new_group_announces_its_genesis_to_the_workspace() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
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
    assert!(ready_epoch(&create(&mut owner, workspace_id, now)).is_some());
    run(&mut [&mut owner], &mut now, 3);
    let genesis: Vec<_> = observer
        .take_inbound()
        .into_iter()
        .filter_map(|inbound| match inbound {
            InboundGovernance::Frame { frame, .. } => Some(frame),
            InboundGovernance::AccessRequest { .. } => None,
        })
        .collect();
    assert_eq!(genesis.len(), 1, "announced once per session");
    assert_eq!(genesis[0].workspace, workspace_id);
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
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(4));
    policy.allowed.lock().unwrap().insert(public(3));
    let (owner_dir, phone_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 3, &policy, owner_dir.path());
    let mut phone = device(&workspace, workspace_id, 1, 4, &policy, phone_dir.path());
    create(&mut owner, workspace_id, now);
    phone.service.access(workspace_id, now);
    // The request gets through, then the network fails before the
    // genesis and the Welcome can. The phone, hearing of no group,
    // creates its own.
    run(&mut [&mut phone], &mut now, 1);
    workspace.down.store(true, Ordering::SeqCst);
    run(&mut [&mut owner, &mut phone], &mut now, 30);
    assert!(!owner.service.peers[&workspace_id].peer.outbox().is_empty());
    workspace.down.store(false, Ordering::SeqCst);
    run(&mut [&mut owner, &mut phone], &mut now, 130);
    let epoch = ready_epoch(&phone.service.access(workspace_id, now));
    assert!(epoch.is_some());
    assert_eq!(ready_epoch(&owner.service.access(workspace_id, now)), epoch);
    assert_eq!(
        owner.service.peers[&workspace_id].peer.members(),
        phone.service.peers[&workspace_id].peer.members(),
        "one group"
    );
    assert!(
        owner.service.peers[&workspace_id].peer.outbox().is_empty(),
        "the outbox drains once the transport takes it"
    );
}

#[test]
fn group_state_and_gates_survive_a_restart() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(6));
    policy.allowed.lock().unwrap().insert(public(5));
    let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    {
        let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
        let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
        owner.service.access(workspace_id, now);
        tablet.service.access(workspace_id, now);
        run(&mut [&mut owner, &mut tablet], &mut now, 5);
        assert!(ready_epoch(&tablet.service.access(workspace_id, now)).is_some());
    }
    for (id, seed, dir) in [(0, 5, owner_dir.path()), (1, 6, tablet_dir.path())] {
        let mut restarted = device(&workspace, workspace_id, id, seed, &policy, dir);
        assert!(
            refuses_plaintext(&restarted),
            "the plaintext gate holds from the first op after a restart"
        );
        assert!(ready_epoch(&restarted.service.access(workspace_id, now)).is_some());
    }
}

/// A key is on disk before it is handed out for sealing: a device that takes
/// in its Welcome, or a commit that moves its group on, and dies the moment
/// after it handed the key out (nothing written on the way out) comes back
/// holding that key, so what it sealed under it opens again.
#[test]
fn a_key_is_written_before_it_is_handed_out() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(6));
    policy.allowed.lock().unwrap().insert(public(5));
    let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 5, &policy, dirs[0].path());
    let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, dirs[1].path());
    let mut phone = device(&workspace, workspace_id, 2, 7, &policy, dirs[2].path());
    owner.service.access(workspace_id, now);
    // The first epoch the tablet is handed, then the next after the phone is
    // admitted: each read right after the pass that took its frame in.
    for admit_phone in [false, true] {
        let before = ready_epoch(&tablet.service.access(workspace_id, now));
        if admit_phone {
            policy.allowed.lock().unwrap().insert(public(7));
            phone.service.access(workspace_id, now);
        }
        let mut handed = before;
        for _ in 0..400 {
            now = now.checked_add(Duration::from_millis(250)).unwrap();
            for device in [&mut owner, &mut tablet, &mut phone] {
                device.service.pump(&device.endpoint, now, 1_000);
            }
            handed = ready_epoch(&tablet.service.access(workspace_id, now));
            if handed != before {
                break;
            }
        }
        assert_ne!(handed, before, "the tablet is handed a new key");
        drop(tablet);
        tablet = device(&workspace, workspace_id, 1, 6, &policy, dirs[1].path());
        assert_eq!(
            ready_epoch(&tablet.service.access(workspace_id, now)),
            handed,
            "the key handed out was written"
        );
    }
}

/// Key loss: a laptop is lost with all its group state. Restoring the
/// person's identity backup on a replacement machine brings back the
/// root identity, not the lost device key; onboarding certifies a new
/// device key. Once the workspace's directory names that certificate, the
/// policy allows the new device, a participant admits it, and its key
/// history opens everything written before the loss. The lost device is
/// removed, so it cannot read what is written afterwards.
#[test]
fn a_restored_identity_regains_the_whole_group_on_its_new_device() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
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
    owner.service.access(workspace_id, now);
    lost.service.access(workspace_id, now);
    run(&mut [&mut owner, &mut lost], &mut now, 5);
    let KeyAccess::Ready(keys) = create(&mut owner, workspace_id, now) else {
        panic!("owner keys");
    };
    let before = keys
        .seal_content(
            artifact,
            collab_types::ContentRetention::Delta,
            b"before the loss",
        )
        .unwrap();
    let KeyAccess::Ready(lost_keys) = lost.service.access(workspace_id, now) else {
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
        restored.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    run(&mut [&mut owner, &mut restored], &mut now, 5);
    let KeyAccess::Ready(restored_keys) = restored.service.access(workspace_id, now) else {
        panic!("the restored identity's new device is admitted");
    };
    assert!(
        restored_keys
            .open_content(artifact, &before.0, &before.1)
            .is_ok(),
        "its key history opens everything written before the loss"
    );
    let KeyAccess::Ready(keys) = create(&mut owner, workspace_id, now) else {
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

/// A machine copied from another re-keys its device. Its copied group
/// state names the source's member, so it is dropped rather than used: the
/// branch takes no leaf of the source's. Once the person's new device is a
/// reader, a participant admits it, its key history opens what was written
/// before, and what either writes afterwards the other reads, while the
/// source stays the member it was.
#[test]
fn a_branched_device_rejoins_as_itself_beside_its_source() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([20]);
    let (source_dir, branch_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut source = device(&workspace, workspace_id, 0, 20, &policy, source_dir.path());
    let KeyAccess::Ready(keys) = create(&mut source, workspace_id, now) else {
        panic!("the source's keys");
    };
    let before = keys
        .seal_content(artifact, ContentRetention::Delta, b"before the copy")
        .unwrap();
    let source_epoch = keys.epoch();

    // The disk is copied; the copy re-keys to device 21 and keeps the
    // person's vault. Certified, it is the person's device and a reader.
    copy_tree(source_dir.path(), branch_dir.path());
    let copied = branch_dir
        .path()
        .join("groups")
        .join(format!("{workspace_id}.peer"));
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
        branch.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    run(&mut [&mut source, &mut branch], &mut now, 5);
    let KeyAccess::Ready(branch_keys) = branch.service.access(workspace_id, now) else {
        panic!("the branch is admitted as itself");
    };
    assert!(
        branch_keys
            .open_content(artifact, &before.0, &before.1)
            .is_ok(),
        "its key history opens what was written before the copy"
    );

    // Each reads what the other writes from now on.
    let KeyAccess::Ready(source_keys) = source.service.access(workspace_id, now) else {
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
        assert!(ready_epoch(&restarted.service.access(workspace_id, now)).is_some());
    }
}

/// A copy the workspace has not admitted yet opens what the copied state's keys open and seals
/// nothing with them; once it is admitted, the state is dropped and it joins as itself.
#[test]
fn a_copy_awaiting_admission_reads_with_the_copied_keys_and_seals_nothing() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([20]);
    let (source_dir, branch_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut source = device(&workspace, workspace_id, 0, 20, &policy, source_dir.path());
    let KeyAccess::Ready(keys) = create(&mut source, workspace_id, now) else {
        panic!("the source's keys");
    };
    let before = keys
        .seal_content(artifact, ContentRetention::Delta, b"before the copy")
        .unwrap();

    copy_tree(source_dir.path(), branch_dir.path());
    let copied = branch_dir
        .path()
        .join("groups")
        .join(format!("{workspace_id}.peer"));
    let mut branch = sealed_device(
        &workspace,
        workspace_id,
        1,
        (20, 21),
        &policy,
        branch_dir.path(),
    );
    assert!(copied.exists(), "the copied state stays until admission");
    let opened = branch
        .service
        .history_keys(workspace_id)
        .unwrap()
        .unwrap()
        .open_content(artifact, &before.0, &before.1)
        .unwrap();
    assert_eq!(opened.as_slice(), b"before the copy");
    assert!(matches!(
        branch.service.access(workspace_id, now),
        KeyAccess::Unavailable
    ));
    assert!(
        !matches!(branch.service.record_keys(workspace_id), Some(Ok(_))),
        "nothing seals with the copied keys"
    );

    policy.allowed.lock().unwrap().insert(public(21));
    assert!(matches!(
        branch.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    assert!(
        branch
            .service
            .history_keys(workspace_id)
            .is_none_or(|keys| keys.is_err()),
        "the copied keys are dropped once it is admitted"
    );
    run(&mut [&mut source, &mut branch], &mut now, 5);
    assert!(ready_epoch(&branch.service.access(workspace_id, now)).is_some());
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
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(12));
    policy.allowed.lock().unwrap().insert(public(11));
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 11, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 12, &policy, laptop_dir.path());
    let KeyAccess::Ready(first) = create(&mut owner, workspace_id, now) else {
        panic!("owner keys");
    };
    let old = sealed_op(
        artifact,
        first
            .seal_content(artifact, ContentRetention::Delta, b"before the rotation")
            .unwrap(),
        OpKind::Change,
    );
    laptop.service.access(workspace_id, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    let keys = SharedKeys::new(owner.service);
    let (KeyAccess::Ready(current), _) = keys.access(workspace_id) else {
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

fn sealed_write(artifact: collab_types::ArtifactId, keys: &ContentKeys) -> SignedOp {
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
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([10]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 10, &policy, owner_dir.path());
    let mut laptop = device(&workspace, workspace_id, 1, 11, &policy, laptop_dir.path());
    let KeyAccess::Ready(before) = create(&mut owner, workspace_id, now) else {
        panic!("the owner holds the group's keys");
    };
    let stale = sealed_write(artifact, &before);
    policy.allowed.lock().unwrap().insert(public(11));
    laptop.service.access(workspace_id, now);
    run(&mut [&mut owner, &mut laptop], &mut now, 5);
    let access = SharedKeys::new(owner.service);
    let (KeyAccess::Ready(current), _) = access.access(workspace_id) else {
        panic!("the owner holds the group's keys");
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
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([12, 13]);
    let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let now = Instant::now();
    let mut owner = device(&workspace, workspace_id, 0, 12, &policy, owner_dir.path());
    let laptop = device(&workspace, workspace_id, 1, 13, &policy, laptop_dir.path());
    let KeyAccess::Ready(keys) = create(&mut owner, workspace_id, now) else {
        panic!("the owner holds the group's keys");
    };
    let write = sealed_write(artifact, &keys);
    let waiting = SharedKeys::new(laptop.service);
    let state = waiting.access(workspace_id).0;
    assert!(matches!(state, KeyAccess::Waiting), "{state:?}");
    assert!(
        waiting.renew_sealed(workspace_id, &write).is_err(),
        "a device still waiting for access renews nothing"
    );
}

#[test]
fn opening_history_never_creates_a_group_without_an_existing_key() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
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
    create(&mut owner, workspace_id, Instant::now());
    let keys = SharedKeys::new(owner.service);
    let (KeyAccess::Ready(held), _) = keys.access(workspace_id) else {
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
    let fresh = SharedKeys::new(fresh.service);
    let generation = fresh.generation();
    assert!(fresh.open_history_write(workspace_id, &sealed).is_err());
    let state = fresh.inner.lock().unwrap();
    assert!(
        state.peers.is_empty(),
        "reading history must not mint replacement keys or request access"
    );
    assert!(
        !state.queued,
        "reading history must not enqueue group governance"
    );
    assert_eq!(state.generation(), generation);
    drop(state);
}

include!("unopened_tests.rs");

/// A device whose secret store did not open keeps no group state of
/// its own. A workspace is answered that it has no secret store; asking again
/// creates nothing, sends nothing and leaves its pump nothing to do, so
/// nobody asks on a timer.
#[test]
fn without_a_store_nothing_is_due() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    // The directory announces policy changes, as a session's does.
    let policy = Policy::default();
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
        std::sync::atomic::AtomicU64::new(1),
    )));
    let gates = Arc::new(Gates::default());
    let mut service = WorkspaceKeys::without_store(
        Box::new(Directory::new(policy.clone())),
        Arc::clone(&gates) as Arc<dyn GateRegistry>,
    );
    let endpoint = Endpoint::join(&workspace, workspace_id, 0);
    let now = Instant::now();
    assert!(matches!(
        service.access(workspace_id, now),
        KeyAccess::NoSecretStore
    ));
    let generation = service.generation();
    for later in 1..=3 {
        let now = now.checked_add(Duration::from_secs(later * 60)).unwrap();
        service.pump(&endpoint, now, 1_000);
        assert!(matches!(
            service.access(workspace_id, now),
            KeyAccess::NoSecretStore
        ));
        assert_eq!(service.next_pump_deadline(now), None, "nothing is due");
    }
    assert_eq!(service.generation(), generation, "nothing changed");
    assert!(service.peers.is_empty());
    assert!(service.existing_keys(workspace_id).is_none());
    assert!(
        workspace.queues.lock().unwrap().values().all(Vec::is_empty),
        "no governance was sent"
    );
    // The workspace is a governed one too: still no state of its own.
    let mut governed = WorkspaceKeys::without_store(Box::new(Directory::new(policy)), gates);
    assert!(matches!(
        governed.access(workspace_id, now),
        KeyAccess::NoSecretStore
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
fn a_reader_with_custody_that_signs_later_reads_a_participants_group() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([31, 32, 33]);
    let (desktop_dir, page_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let reader = |seed: u8, id: usize, dir: &Path| {
        workspace.queues.lock().unwrap().entry(id).or_default();
        Device {
            founds: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            service: WorkspaceKeys::open(
                KeysConfig {
                    state: Arc::new(KeyDir::new(dir.join("groups"))),
                    vault: VaultKey::new([seed; 32]),
                    signer: Arc::new(LaterSigner(key(seed))),
                    role: KeyRole::Follower,
                },
                Box::new(Directory::new(policy.clone())),
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
        page.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    run(&mut [&mut page], &mut now, 30);
    assert!(!page.service.peers[&workspace_id].peer.knows_group());
    assert!(page.service.signing(), "its request waits on its custody");
    pollster::block_on(page.service.signatures());
    assert!(!page.service.signing());

    let mut desktop = device(&workspace, workspace_id, 0, 31, &policy, desktop_dir.path());
    let KeyAccess::Ready(_) = create(&mut desktop, workspace_id, now) else {
        panic!("the desktop founds the group");
    };
    for _ in 0..10 {
        run(&mut [&mut desktop, &mut page], &mut now, 3);
        pollster::block_on(page.service.signatures());
    }
    let KeyAccess::Ready(keys) = page.service.access(workspace_id, now) else {
        panic!("the desktop admits the reader");
    };
    let KeyAccess::Ready(desktop_keys) = desktop.service.access(workspace_id, now) else {
        panic!("desktop keys");
    };
    assert_eq!(keys.epoch(), desktop_keys.epoch());
    let sealed = sealed_write(artifact, &desktop_keys);
    assert_eq!(
        SharedKeys::new(page.service)
            .open_new_write(workspace_id, &sealed)
            .unwrap(),
        b"the edit"
    );

    // The policy admits a third device that asks: the desktop commits it, never a reader.
    let mut page = reader(32, 1, page_dir.path());
    let third_dir = tempfile::tempdir().unwrap();
    let mut third = reader(33, 2, third_dir.path());
    third.service.access(workspace_id, now);
    page.service.access(workspace_id, now);
    for _ in 0..10 {
        run(&mut [&mut page, &mut third], &mut now, 3);
        pollster::block_on(third.service.signatures());
        assert!(page.service.peers[&workspace_id].peer.outbox().is_empty());
    }
    assert!(matches!(
        third.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    run(&mut [&mut desktop, &mut page, &mut third], &mut now, 10);
    pollster::block_on(third.service.signatures());
    run(&mut [&mut desktop, &mut page, &mut third], &mut now, 10);
    assert_eq!(
        ready_epoch(&third.service.access(workspace_id, now)),
        ready_epoch(&page.service.access(workspace_id, now)),
    );
}

/// What `device` heard the workspace publish, drained: how many access
/// requests reached it.
fn requests(device: &Device) -> usize {
    device
        .endpoint
        .take_inbound()
        .iter()
        .filter(|heard| matches!(heard, InboundGovernance::AccessRequest { .. }))
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
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([2]);
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
    *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
    let (dir, listener_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut waiting = device(&workspace, workspace_id, 0, 2, &policy, dir.path());
    // A group the device did not create and is not in yet.
    waiting.founds.store(false, Ordering::SeqCst);
    // Another endpoint hears what the device says; it takes no part in the
    // group, so the request goes unanswered.
    let listener = device(&workspace, workspace_id, 1, 3, &policy, listener_dir.path());
    let mut now = Instant::now();
    assert!(matches!(
        waiting.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    waiting.service.pump(&waiting.endpoint, now, 1_000);
    let mut asked = requests(&listener);
    assert_eq!(asked, 1, "opening the group asks");

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

/// Two groups of one group: the members of the one whose genesis lost
/// start over and join the winner, and every member reads what the others
/// seal from then on, and what was sealed on either side before.
#[test]
fn every_member_of_a_group_that_forked_reads_the_winners_records() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let artifact = collab_types::ArtifactId::new();
    let policy = Policy::readers([21, 22, 23]);
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut now = Instant::now();
    let mut alone = device(&workspace, workspace_id, 0, 21, &policy, dirs[0].path());
    let mut founder = device(&workspace, workspace_id, 1, 22, &policy, dirs[1].path());
    let mut joiner = device(&workspace, workspace_id, 2, 23, &policy, dirs[2].path());
    // One device founds a group it publishes to no one yet; two others found
    // and join another, and seal under it.
    let KeyAccess::Ready(early) = create(&mut alone, workspace_id, now) else {
        panic!("a founder holds its group's keys");
    };
    let before_alone = early
        .seal_content(artifact, ContentRetention::Delta, b"sealed alone")
        .unwrap();
    create(&mut founder, workspace_id, now);
    joiner.service.access(workspace_id, now);
    run(&mut [&mut founder, &mut joiner], &mut now, 10);
    let KeyAccess::Ready(pair) = joiner.service.access(workspace_id, now) else {
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
            .map(|device| ready_epoch(&device.service.access(workspace_id, now)))
            .collect();
        let members: Vec<_> = devices
            .iter()
            .map(|device| device.service.peers[&workspace_id].peer.members())
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
    eprintln!("a forked group converged in {converged:?} of simulated time");

    let keys: Vec<ContentKeys> = devices
        .iter_mut()
        .map(|device| match device.service.access(workspace_id, now) {
            KeyAccess::Ready(keys) => keys,
            other => panic!("every member reads the group: {other:?}"),
        })
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
        let KeyAccess::Ready(keys) = device.service.access(workspace_id, now) else {
            panic!("every member still reads the group");
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
    let policy = Policy::readers([31, 32]);
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut now = Instant::now();
    let mut founder = device(&workspace, workspace_id, 0, 31, &policy, dirs[0].path());
    let mut reader = device(&workspace, workspace_id, 1, 32, &policy, dirs[1].path());
    create(&mut founder, workspace_id, now);
    run(&mut [&mut founder], &mut now, 1);
    // The reader hears the group and asks to join; the founder is away.
    reader.service.access(workspace_id, now);
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
        if ready_epoch(&reader.service.access(workspace_id, now)).is_some() {
            break;
        }
    }
    let joined = now.duration_since(back);
    assert!(
        ready_epoch(&reader.service.access(workspace_id, now)).is_some(),
        "the reader joins"
    );
    assert!(
        joined <= Duration::from_secs(3),
        "the reader asks again as soon as the stale admission reaches it, joined after {joined:?}"
    );
}

/// A device that cannot name a group's record keeps a genesis competing
/// with its group's until it can, and then moves to the winner's group.
#[test]
fn a_competing_genesis_is_judged_once_the_device_names_the_record() {
    let workspace_id = WorkspaceId::new();
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
        create(&mut founder, workspace_id, now);
        run(&mut [&mut founder], &mut now.clone(), 1);
        let genesis = listener
            .take_inbound()
            .into_iter()
            .find_map(|inbound| match inbound {
                InboundGovernance::Frame { frame, .. } => Some(*frame),
                InboundGovernance::AccessRequest { .. } => None,
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
    reader.service.access(workspace_id, now);
    run(&mut [&mut loser, &mut reader], &mut now, 10);
    let epoch = ready_epoch(&reader.service.access(workspace_id, now));
    assert!(epoch.is_some(), "the reader joins the loser's group");

    // The winner's genesis reaches the reader while it cannot name the record.
    reader_policy.unknown.store(true, Ordering::SeqCst);
    workspace
        .queues
        .lock()
        .unwrap()
        .entry(1)
        .or_default()
        .push(InboundGovernance::Frame {
            workspace: workspace_id,
            frame: Box::new(winning),
        });
    run(&mut [&mut reader], &mut now, 2);
    assert_eq!(
        ready_epoch(&reader.service.access(workspace_id, now)),
        epoch,
        "a genesis the reader cannot judge does not move it"
    );

    // Named again, the winner's genesis decides: the reader starts over.
    reader_policy.unknown.store(false, Ordering::SeqCst);
    run(&mut [&mut reader], &mut now, 1);
    assert!(
        reader.service.peers[&workspace_id].peer.access() == Access::Waiting,
        "the reader leaves the group whose genesis lost"
    );
}
