use super::*;
use atomic_file::StorageDirectory;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use ui::host_worker::private_actions::{
    SenderScope,
    administration::{AdministrativeExpectation, AdministrativeFile},
};

fn fixture() -> tempfile::TempDir {
    let temp = match std::env::var_os("QOS_PRIVATE_ACTIONS_TEST_ROOT") {
        Some(parent) => {
            let parent = std::path::PathBuf::from(parent);
            assert!(
                parent.is_absolute(),
                "explicit fixture root must be absolute"
            );
            assert!(parent.is_dir(), "explicit fixture root must already exist");
            tempfile::tempdir_in(parent).unwrap()
        }
        None => tempfile::tempdir().unwrap(),
    };
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let opened = std::fs::File::open(temp.path()).unwrap();
    assert_eq!(
        opened.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    #[cfg(target_os = "linux")]
    if std::env::var_os("QOS_PRIVATE_ACTIONS_TEST_ROOT").is_some() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(opened.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        let stat = unsafe { stat.assume_init() };
        assert_eq!(stat.f_type as u64, 0x5846_5342, "actual XFS required");
        let metadata = opened.metadata().unwrap();
        eprintln!(
            "native-sender-admin XFS fixture={} device={} inode={} magic={:#x}",
            temp.path().display(),
            metadata.dev(),
            metadata.ino(),
            stat.f_type
        );
    }
    temp
}

#[derive(Serialize, Deserialize)]
struct SelectedWire {
    version: u32,
    base: Vec<u8>,
    sender: Vec<u8>,
}

#[test]
fn sender_admin_managed_init_commits_active_real_file() {
    let container = fixture();
    let root = container.path().join("owner");
    let result = initialize(&root);
    // Even the initial refusal scaffold runs the genuine identity/seed/Store
    // initializer. Its failure must not be a missing API or a synthetic owner.
    assert!(root.join("session.bin").is_file());
    assert!(root.join("identity").is_dir());
    assert!(root.join("host-identity").is_dir());
    let status = result.expect("real managed initializer must commit Active storage");
    assert_eq!(status.phase, AdminPhase::Active);

    let bytes = std::fs::read(root.join("session.bin")).unwrap();
    let signed: super::super::SignedAggregate = postcard::from_bytes(&bytes).unwrap();
    super::super::schema::canonical(&signed, &bytes).unwrap();
    let outer: SelectedWire = postcard::from_bytes(&signed.payload).unwrap();
    super::super::schema::canonical(&outer, &signed.payload).unwrap();
    assert_eq!(outer.version, 7);
    assert_eq!(outer.base[0], 4);
    assert_eq!(outer.sender.len(), 187);
    assert_eq!(&outer.sender[..3], &[1, 1, 1]);
    assert_eq!(&outer.sender[3..19], &status.room);
    assert_eq!(&outer.sender[19..51], &status.owner_root);
    assert_eq!(&outer.sender[51..83], &status.native_device);
    assert_eq!(&outer.sender[83..99], &status.instance);
    assert_ne!(status.instance, [0; 16]);
    assert_eq!(&outer.sender[99..107], &1_u64.to_le_bytes());
    assert_eq!(&outer.sender[107..115], &0_u64.to_le_bytes());
    assert_eq!(
        &outer.sender[115..147],
        blake3::hash(b"NSO1\x01\x00\x00\x00").as_bytes()
    );

    let aggregate = super::super::schema::Aggregate::decode_payload(&outer.base).unwrap();
    let identity =
        crate::identity::load(&aggregate.identity_root, &aggregate.host_identity_root).unwrap();
    super::super::restore(&aggregate, &signed, &identity).unwrap();
    assert_eq!(status.owner_root, identity.native_cert.user_public_key);
    assert_eq!(status.native_device, identity.native_cert.device_public_key);
    assert_ne!(status.native_device, identity.host_cert.device_public_key);
    assert_eq!(
        uuid::Uuid::parse_str(&aggregate.room).unwrap().as_bytes(),
        &status.room
    );
    assert_eq!(aggregate.generation, 1);

    let inventory = AdminInventory::new().unwrap();
    let scope = SenderScope::native(
        uuid::Uuid::from_bytes(status.room),
        status.owner_root,
        status.native_device,
    )
    .unwrap();
    let credit = inventory
        .allowance
        .reserve(AdministrativeFile::required_bytes(&scope).unwrap())
        .unwrap();
    let file = AdministrativeFile::open(
        StorageDirectory::open(&root).unwrap(),
        scope,
        status.instance,
        AdministrativeExpectation::Active,
        credit,
    )
    .unwrap();
    let facts = file.revalidate().unwrap();
    assert_eq!(facts.instance(), &status.instance);
    assert_eq!(facts.sequence(), 1);
    assert_eq!(facts.revision(), 0);
    let physical = std::fs::read(root.join("private-actions-v1/private-actions-v1.bin")).unwrap();
    assert_eq!(physical.len(), 439);
    assert_eq!(facts.file_digest(), blake3::hash(&physical).as_bytes());
}

use atomic_file::test_support::{self, Checkpoint};
use std::cell::RefCell;
thread_local! {
    static ACTION: RefCell<Option<(Boundary, Box<dyn FnOnce(&crate::store::SessionStore)>)>> = const { RefCell::new(None) };
    static COMMIT: RefCell<Option<(u8, Checkpoint)>> = const { RefCell::new(None) };
    static PHYSICAL: RefCell<Option<(Checkpoint, usize)>> = const { RefCell::new(None) };
}
pub(super) fn boundary(at: Boundary, store: &crate::store::SessionStore) {
    let action = ACTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(point, _)| *point == at) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, action)) = action {
        action(store);
    }
}
pub(super) fn commit(
    phase: u8,
    store: &mut crate::store::SessionStore,
    bytes: &[u8],
) -> Result<()> {
    let fault = COMMIT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(p, _)| *p == phase) {
            slot.take()
        } else {
            None
        }
    });
    match fault {
        Some((_, point)) => test_support::with_failure(point, || store.commit(bytes)),
        None => store.commit(bytes),
    }
}
pub(super) fn physical<T>(operation: impl FnOnce() -> T) -> T {
    match PHYSICAL.with(|slot| slot.borrow_mut().take()) {
        Some((point, nth)) => test_support::with_failure_on_nth(point, nth, operation),
        None => operation(),
    }
}
fn unselected(root: &Path) -> Vec<u8> {
    drop(SessionOwner::initialize(root).unwrap());
    std::fs::read(root.join("session.bin")).unwrap()
}
fn outer(root: &Path) -> (Vec<u8>, SelectedWire) {
    let raw = std::fs::read(root.join("session.bin")).unwrap();
    let signed: super::super::SignedAggregate = postcard::from_bytes(&raw).unwrap();
    (raw, postcard::from_bytes(&signed.payload).unwrap())
}
fn interrupt_prepared(
    root: &Path,
    action: impl FnOnce(&crate::store::SessionStore) + 'static,
) -> SelectedWire {
    ACTION.with(|slot| {
        *slot.borrow_mut() = Some((
            Boundary::AfterPrepared,
            Box::new(move |store| {
                action(store);
                panic!("test process interruption after durable Prepared");
            }),
        ))
    });
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| select(root))).is_err());
    let (_, wire) = outer(root);
    assert_eq!(wire.sender[1], 0);
    wire
}

#[test]
fn sender_admin_named_initializer_and_status_reopen_are_inert() {
    let container = fixture();
    let root = container.path().join("named");
    let initialized = initialize_workspace(&root, "Private workspace").unwrap();
    let before = std::fs::read(root.join("session.bin")).unwrap();
    for observed in [status(&root).unwrap(), resume(&root).unwrap()] {
        assert_eq!(observed.phase, AdminPhase::Active);
        assert_eq!(observed.instance, initialized.instance);
        assert_eq!(observed.room, initialized.room);
    }
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
    assert!(select(&root).is_err());
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
}

#[test]
fn sender_admin_selection_preserves_exact_base_generation_and_prior_digest() {
    let container = fixture();
    let root = container.path().join("owner");
    let prior = unselected(&root);
    let signed: super::super::SignedAggregate = postcard::from_bytes(&prior).unwrap();
    let prepared = interrupt_prepared(&root, |_| {});
    assert_eq!(prepared.base, signed.payload);
    assert_eq!(&prepared.sender[147..155], &1_u64.to_le_bytes());
    assert_eq!(&prepared.sender[155..187], blake3::hash(&prior).as_bytes());
    assert!(!root.join("private-actions-v1.writer.lock").exists());
    assert!(!root.join("private-actions-v1").exists());
    let inert = status(&root).unwrap();
    assert_eq!(inert.phase, AdminPhase::Prepared);
    assert!(!root.join("private-actions-v1.writer.lock").exists());
    let active = resume(&root).unwrap();
    assert_eq!(active.instance.as_slice(), &prepared.sender[83..99]);
    let (_, after) = outer(&root);
    let mut expected = prepared.sender;
    expected[1] = 1;
    assert_eq!(after.sender, expected);
    assert_eq!(after.base, signed.payload);
}

#[test]
fn sender_admin_ordinary_open_encode_and_candidate_change_stay_closed() {
    let container = fixture();
    let root = container.path().join("owner");
    initialize(&root).unwrap();
    let (before, _) = outer(&root);
    let (aggregate, _) = super::super::decode(&before).unwrap();
    assert!(aggregate.encode_payload().is_err());
    assert!(SessionOwner::open(&root).is_err());
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
    let root2 = container.path().join("ordinary");
    let mut ordinary = SessionOwner::initialize(&root2).unwrap();
    let mut candidate = ordinary.aggregate.clone();
    candidate.sender = aggregate.sender;
    assert!(ordinary.commit(candidate, ordinary.peer.clone()).is_err());
    assert_eq!(ordinary.aggregate.sender, None);
}

#[test]
fn sender_admin_actual_prepared_barrier_failure_retains_original_error_and_owner() {
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let container = fixture();
        let root = container.path().join("owner");
        let before = unselected(&root);
        COMMIT.with(|slot| *slot.borrow_mut() = Some((0, point)));
        let failure = select(&root).unwrap_err();
        assert!(failure.publication_error().is_some());
        let held = failure.operation.as_ref().unwrap();
        assert!(held.store.is_poisoned());
        assert!(held.output.is_some());
        assert!(crate::store::SessionStore::open(&root).is_err());
        assert!(!root.join("private-actions-v1.writer.lock").exists());
        drop(failure);
        let current = std::fs::read(root.join("session.bin")).unwrap();
        if current == before {
            select(&root).unwrap();
        } else {
            resume(&root).unwrap();
        }
        assert_eq!(status(&root).unwrap().phase, AdminPhase::Active);
    }
}

#[test]
fn sender_admin_actual_active_barrier_failure_keeps_physical_lease_until_drop() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    COMMIT.with(|slot| *slot.borrow_mut() = Some((1, Checkpoint::BeforeCommit)));
    let failure = select(&root).unwrap_err();
    assert!(failure.publication_error().is_some());
    let held = failure.operation.as_ref().unwrap();
    let instance = held.status().instance;
    assert!(held.physical.is_some());
    assert!(held.output.is_some());
    assert!(held.store.is_poisoned());
    let bounded = BoundedOwnerDirectory::admit(StorageDirectory::open(&root).unwrap()).unwrap();
    assert!(matches!(
        atomic_file::lease::FileLease::try_acquire(
            bounded
                .open_lease_existing(OsStr::new("private-actions-v1.writer.lock"))
                .unwrap(),
            atomic_file::lease::LeaseMode::Exclusive
        ),
        Err(atomic_file::lease::AcquireError::Busy)
    ));
    drop(failure);
    assert_eq!(resume(&root).unwrap().instance, instance);
}

#[test]
fn sender_admin_physical_uncertainty_retains_typed_failure_and_same_instance() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    PHYSICAL.with(|slot| *slot.borrow_mut() = Some((Checkpoint::BeforeCommit, 2)));
    let failure = select(&root).unwrap_err();
    assert!(failure.publication_error().is_some());
    assert!(
        failure
            .operation
            .as_ref()
            .unwrap()
            .physical_failure
            .is_some()
    );
    let instance = failure.operation.as_ref().unwrap().status().instance;
    assert!(crate::store::SessionStore::open(&root).is_err());
    drop(failure);
    assert_eq!(resume(&root).unwrap().instance, instance);
}

#[test]
fn sender_admin_active_missing_objects_never_reseed() {
    for missing in [
        "private-actions-v1.writer.lock",
        "private-actions-v1/private-actions-v1.bin",
        "private-actions-v1",
    ] {
        let container = fixture();
        let root = container.path().join("owner");
        initialize(&root).unwrap();
        let before = std::fs::read(root.join("session.bin")).unwrap();
        let path = root.join(missing);
        if path.is_dir() {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        assert!(status(&root).is_err());
        assert!(resume(&root).is_err());
        assert!(select(&root).is_err());
        assert!(!path.exists());
        assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
    }
}

#[test]
fn sender_admin_prepared_unexplained_empty_child_is_not_adopted() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    let prepared = interrupt_prepared(&root, |store| {
        let child = store.root().join("private-actions-v1");
        std::fs::create_dir(&child).unwrap();
        std::fs::set_permissions(child, std::fs::Permissions::from_mode(0o700)).unwrap();
    });
    assert!(resume(&root).is_err());
    assert!(
        !root
            .join("private-actions-v1/private-actions-v1.bin")
            .exists()
    );
    assert_eq!(outer(&root).1.sender, prepared.sender);
}

#[test]
fn sender_admin_original_root_rename_does_not_write_replacement() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    let renamed = container.path().join("renamed");
    let renamed_hook = renamed.clone();
    ACTION.with(|slot| {
        *slot.borrow_mut() = Some((
            Boundary::AfterPrepared,
            Box::new(move |store| {
                std::fs::rename(store.root(), &renamed_hook).unwrap();
                std::fs::create_dir(store.root()).unwrap();
                std::fs::set_permissions(store.root(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                std::fs::write(store.root().join("unrelated"), b"keep").unwrap();
            }),
        ))
    });
    let active = select(&root).unwrap();
    assert_eq!(active.phase, AdminPhase::Active);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(std::fs::read(root.join("unrelated")).unwrap(), b"keep");
    assert!(
        renamed
            .join("private-actions-v1/private-actions-v1.bin")
            .is_file()
    );
    // Existing identity paths intentionally remain ordinary pathname state;
    // this proof concerns effects under the original Store, not identity migration.
}

#[test]
fn sender_admin_final_file_change_refuses_success() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    ACTION.with(|slot| {
        *slot.borrow_mut() = Some((
            Boundary::AfterActive,
            Box::new(|store| {
                use std::io::{Seek, Write};
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(
                        store
                            .root()
                            .join("private-actions-v1/private-actions-v1.bin"),
                    )
                    .unwrap();
                file.seek(std::io::SeekFrom::Start(0)).unwrap();
                file.write_all(b"changed").unwrap();
                file.sync_all().unwrap();
            }),
        ))
    });
    let failure = select(&root).unwrap_err();
    assert_eq!(
        std::error::Error::source(&failure)
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert!(
        failure
            .operation
            .as_ref()
            .unwrap()
            .physical_failure
            .is_some()
    );
    drop(failure);
    assert_eq!(outer(&root).1.sender[1], 1);
    assert!(status(&root).is_err());
}

#[test]
fn sender_admin_exact_outputs_are_prepaid_from_original_inventory() {
    let container = fixture();
    let root = container.path().join("owner");
    let original = unselected(&root);
    let signed: super::super::SignedAggregate = postcard::from_bytes(&original).unwrap();
    let prepared = interrupt_prepared(&root, |_| {});
    let aggregate = schema::Aggregate::decode_payload(&prepared.base).unwrap();
    let identity =
        crate::identity::load(&aggregate.identity_root, &aggregate.host_identity_root).unwrap();
    let inventory = AdminInventory::new().unwrap();
    let selected = schema::SenderSelection::decode(&prepared.sender).unwrap();
    let before = inventory.allowance.available_bytes();
    let encoded =
        encode_selected(&signed.payload, &selected, &identity, &inventory.allowance).unwrap();
    let required = encoded._payload.capacity() + encoded.signed.capacity() + 48;
    assert_eq!(encoded._credit.bytes(), required);
    assert_eq!(before - inventory.allowance.available_bytes(), required);
    assert_eq!(encoded._payload.len(), encoded._payload.capacity());
    assert_eq!(encoded.signed.len(), encoded.signed.capacity());
    let held = inventory
        .allowance
        .reserve(inventory.allowance.available_bytes())
        .unwrap();
    assert!(encode_selected(&signed.payload, &selected, &identity, &inventory.allowance).is_err());
    drop(held);
    drop(encoded);
    assert_eq!(before, inventory.allowance.available_bytes());
    eprintln!(
        "native sender outputs: exact W+S+48={}, holder={}, original-pool allowance restored",
        required,
        std::mem::size_of::<Encoded>()
    );
}

#[test]
fn sender_admin_strict_outer_inner_and_fixed_selection_rejections() {
    let container = fixture();
    let root = container.path().join("owner");
    initialize(&root).unwrap();
    let (raw, wire) = outer(&root);
    let valid = postcard::to_stdvec(&wire).unwrap();
    for offset in [0, 1, 2, 3, 19, 51, 83, 99, 107, 115, 147] {
        let mut bad = SelectedWire {
            version: 7,
            base: wire.base.clone(),
            sender: wire.sender.clone(),
        };
        match offset {
            0..=2 => bad.sender[offset] = 9,
            83 => bad.sender[83..99].fill(0),
            _ => bad.sender[offset] ^= 1,
        }
        assert!(
            schema::Aggregate::decode_administrative_payload(&postcard::to_stdvec(&bad).unwrap())
                .is_err(),
            "offset {offset}"
        );
    }
    for length in [0, 186, 188] {
        let bad = SelectedWire {
            version: 7,
            base: wire.base.clone(),
            sender: vec![1; length],
        };
        assert!(
            schema::Aggregate::decode_administrative_payload(&postcard::to_stdvec(&bad).unwrap())
                .is_err()
        );
    }
    let nested = SelectedWire {
        version: 7,
        base: valid.clone(),
        sender: wire.sender.clone(),
    };
    assert!(
        schema::Aggregate::decode_administrative_payload(&postcard::to_stdvec(&nested).unwrap())
            .is_err()
    );
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(schema::Aggregate::decode_administrative_payload(&trailing).is_err());
    let mut overlong = vec![0x87, 0];
    overlong.extend_from_slice(&valid[1..]);
    assert!(schema::Aggregate::decode_administrative_payload(&overlong).is_err());
    let mut inner = vec![0x84, 0];
    inner.extend_from_slice(&wire.base[1..]);
    let bad = SelectedWire {
        version: 7,
        base: inner,
        sender: wire.sender.clone(),
    };
    assert!(
        schema::Aggregate::decode_administrative_payload(&postcard::to_stdvec(&bad).unwrap())
            .is_err()
    );
    let mut signed_trailing = raw;
    signed_trailing.push(0);
    assert!(super::super::decode(&signed_trailing).is_err());
}

fn sign_and_commit(root: &Path, payload: Vec<u8>, identity: &crate::identity::Identity) -> Vec<u8> {
    let signature = identity
        .native
        .sign(
            blake3::Hasher::new_derive_key("qOS native session aggregate v1")
                .update(&payload)
                .finalize()
                .as_bytes(),
        )
        .to_bytes()
        .to_vec();
    let signed =
        postcard::to_stdvec(&super::super::SignedAggregate { payload, signature }).unwrap();
    crate::store::SessionStore::open(root)
        .unwrap()
        .commit(&signed)
        .unwrap();
    signed
}
#[test]
fn sender_admin_authenticated_host_certificate_substitution_refuses_without_rewrite() {
    let container = fixture();
    let root = container.path().join("owner");
    initialize(&root).unwrap();
    let (_, mut wire) = outer(&root);
    let base = schema::Aggregate::decode_payload(&wire.base).unwrap();
    let identity = crate::identity::load(&base.identity_root, &base.host_identity_root).unwrap();
    wire.sender[51..83].copy_from_slice(&identity.host_cert.device_public_key);
    let bad = sign_and_commit(&root, postcard::to_stdvec(&wire).unwrap(), &identity);
    drop(identity);
    assert!(status(&root).is_err());
    assert!(resume(&root).is_err());
    assert_eq!(bad, std::fs::read(root.join("session.bin")).unwrap());
}

#[test]
fn sender_admin_noncanonical_old_v4_is_explicitly_ineligible() {
    let container = fixture();
    let root = container.path().join("owner");
    let before = unselected(&root);
    let signed: super::super::SignedAggregate = postcard::from_bytes(&before).unwrap();
    let base = schema::Aggregate::decode_payload(&signed.payload).unwrap();
    let identity = crate::identity::load(&base.identity_root, &base.host_identity_root).unwrap();
    let mut overlong = vec![0x84, 0];
    overlong.extend_from_slice(&signed.payload[1..]);
    assert!(
        schema::Aggregate::decode_payload(&overlong).is_ok(),
        "historical V4 decoder remains unchanged"
    );
    let bad = sign_and_commit(&root, overlong, &identity);
    drop(identity);
    assert!(select(&root).is_err());
    assert_eq!(bad, std::fs::read(root.join("session.bin")).unwrap());
    assert!(!root.join("private-actions-v1.writer.lock").exists());
    assert!(SessionOwner::open(&root).is_ok());
}

#[test]
fn sender_admin_v5_v6_exact_borrowed_base_and_delivery_generation_preserved() {
    use tilo_relay::delivery_v3::commit::{DeliveryLedgerV1, PublicationPool, PublicationRef};
    for version in [5, 6] {
        let container = fixture();
        let root = container.path().join("owner");
        let mut owner = SessionOwner::initialize(&root).unwrap();
        let mut pool = PublicationPool::new(crate::store::MAX_AGGREGATE_BYTES).unwrap();
        let roots = owner
            .aggregate
            .outgoing()
            .iter()
            .map(|p| PublicationRef {
                payload: pool.intern(p.bytes).unwrap(),
                kind: p.kind,
            })
            .collect::<Vec<_>>();
        if version == 6 {
            owner.peer.select_admission_v5().unwrap();
            owner.aggregate.peer = owner.peer.durable().unwrap();
            owner.aggregate.creation = Some(Default::default());
        }
        owner.aggregate.publication = schema::PublicationStorage::V5 {
            pool,
            roots: schema::PublicationRootsV1 {
                baseline_generation: owner.aggregate.generation,
                baseline: roots,
                tail: vec![],
            },
            delivery: DeliveryLedgerV1 {
                version: 1,
                store_generation: [7; 16],
                resolved_through: owner.aggregate.generation,
                replay: Default::default(),
                admissions: Default::default(),
                consumers: Default::default(),
                pins: Default::default(),
            },
            transport_hint: None,
        };
        let before = super::super::encode(&owner.aggregate, &owner.identity).unwrap();
        owner.store.commit(&before).unwrap();
        drop(owner);
        let before_signed: super::super::SignedAggregate = postcard::from_bytes(&before).unwrap();
        assert_eq!(before_signed.payload[0], version);
        select(&root).unwrap();
        let (_, after) = outer(&root);
        assert_eq!(after.base, before_signed.payload);
        let aggregate =
            schema::Aggregate::decode_administrative_payload(&postcard::to_stdvec(&after).unwrap())
                .unwrap();
        let schema::PublicationStorage::V5 { delivery, .. } = aggregate.publication else {
            panic!("lost V5");
        };
        assert_eq!(delivery.resolved_through, aggregate.generation);
        assert_eq!(aggregate.creation.is_some(), version == 6);
    }
}

#[test]
fn sender_admin_total_signed_cap_and_overlap_fail_before_output_allocation() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    let prepared = interrupt_prepared(&root, |_| {});
    let base = schema::Aggregate::decode_payload(&prepared.base).unwrap();
    let identity = crate::identity::load(&base.identity_root, &base.host_identity_root).unwrap();
    let selected = schema::SenderSelection::decode(&prepared.sender).unwrap();
    let inventory = AdminInventory::new().unwrap();
    let before = inventory.allowance.available_bytes();
    // Inert test input only: the private encoder is normally passed the actual
    // authenticated base. Both failures occur in count/admission before output.
    let large = vec![0; crate::store::MAX_AGGREGATE_BYTES];
    assert!(encode_selected(&large, &selected, &identity, &inventory.allowance).is_err());
    assert!(
        encode_selected(
            &large[..40 * 1024 * 1024],
            &selected,
            &identity,
            &inventory.allowance
        )
        .is_err()
    );
    assert_eq!(before, inventory.allowance.available_bytes());
}

#[test]
fn sender_admin_capacity_and_unexplained_unselected_artifacts_precede_prepared() {
    let container = fixture();
    let root = container.path().join("owner");
    let before = unselected(&root);
    let inventory = AdminInventory::new().unwrap();
    let pressure = inventory
        .allowance
        .reserve(inventory.allowance.available_bytes())
        .unwrap();
    let operation = Operation::existing(&root, inventory).unwrap();
    let failure = operation.execute(Intent::Select).unwrap_err();
    assert!(!root.join("private-actions-v1.writer.lock").exists());
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
    drop(failure);
    drop(pressure);
    std::fs::create_dir(root.join("private-actions-v1")).unwrap();
    std::fs::set_permissions(
        root.join("private-actions-v1"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(select(&root).is_err());
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
    assert!(!root.join("private-actions-v1.writer.lock").exists());
}

// Independent inert test bytes, subsequently checked by the real SDK codec.
// This neither creates a committed sender row nor returns a result capability.
fn nonempty_observation() -> Vec<u8> {
    fn short(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend_from_slice(&(text.len() as u16).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }
    let mut row = vec![8; 16];
    row.extend_from_slice(&[1; 32]);
    row.extend_from_slice(&[2; 32]);
    short(&mut row, "caller");
    short(&mut row, "record");
    row.extend_from_slice(&u16::MAX.to_le_bytes());
    row.extend_from_slice(&[3; 16]);
    row.extend_from_slice(&7u64.to_le_bytes());
    short(&mut row, "target");
    short(&mut row, "record");
    row.extend_from_slice(&[4; 32]);
    row.extend_from_slice(&[5; 32]);
    row.extend_from_slice(&[6; 32]);
    short(&mut row, "revision");
    short(&mut row, "resource");
    short(&mut row, "action");
    row.extend_from_slice(&u32::MAX.to_le_bytes());
    row.push(1);
    row.extend_from_slice(&9u64.to_le_bytes());
    row.extend_from_slice(&[0, 0]);
    let json = br#"{"state":"succeeded","invocation":"08080808-0808-0808-0808-080808080808","output":{"value":"kept"}}"#;
    row.extend_from_slice(&(json.len() as u32).to_le_bytes());
    row.extend_from_slice(json);
    let mut bytes = b"NSO1\x01\x00\x01\x00".to_vec();
    bytes.extend_from_slice(&(row.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&row);
    bytes
}
fn replace_payload(root: &Path, payload: &[u8]) -> Vec<u8> {
    let child = root.join("private-actions-v1");
    let mut bytes = std::fs::read(child.join("private-actions-v1.bin")).unwrap();
    bytes.truncate(399);
    bytes[379..387].copy_from_slice(&27u64.to_le_bytes());
    bytes[387..395].copy_from_slice(&3u64.to_le_bytes());
    bytes[395..399].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    let digest = blake3::derive_key("qos private sender file envelope v1", &bytes);
    bytes.extend_from_slice(&digest);
    BoundedOwnerDirectory::admit(StorageDirectory::open(&child).unwrap())
        .unwrap()
        .publish(
            OsStr::new("private-actions-v1.bin"),
            &bytes,
            atomic_file::PublishMode::Replace,
        )
        .unwrap();
    bytes
}
#[test]
fn sender_admin_active_accepts_later_valid_payload_but_prepared_does_not() {
    for prepared in [false, true] {
        let container = fixture();
        let root = container.path().join("owner");
        unselected(&root);
        if prepared {
            PHYSICAL.with(|slot| *slot.borrow_mut() = Some((Checkpoint::BeforeCommit, 2)));
            drop(select(&root).unwrap_err());
            assert_eq!(outer(&root).1.sender[1], 0);
        } else {
            select(&root).unwrap();
        }
        let before = std::fs::read(root.join("session.bin")).unwrap();
        let payload = nonempty_observation();
        let physical = replace_payload(&root, &payload);
        if prepared {
            assert!(resume(&root).is_err());
        } else {
            assert_eq!(status(&root).unwrap().phase, AdminPhase::Active);
            assert_eq!(resume(&root).unwrap().phase, AdminPhase::Active);
        }
        assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
        assert_eq!(
            physical,
            std::fs::read(root.join("private-actions-v1/private-actions-v1.bin")).unwrap()
        );
    }
}

// Only new post-admission output/error work is observed. Genuine legacy owner
// admission and the actual ingress/allowance construction precede this scope.
// Those original values remain alive until after the observer is disabled.
use super::super::native_credit::{Stats, allocations_only, snapshot};

fn assert_no_new_backing(observed: Stats) {
    assert_eq!(
        observed.allocations, 0,
        "unexpected new backing: {observed:?}"
    );
    assert_eq!(observed.live, 0);
    assert!(!observed.unmatched_free);
}

#[test]
fn sender_admin_actual_output_allocations_drop_before_original_credit() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    let prepared = interrupt_prepared(&root, |_| {});
    let aggregate = schema::Aggregate::decode_payload(&prepared.base).unwrap();
    let identity =
        crate::identity::load(&aggregate.identity_root, &aggregate.host_identity_root).unwrap();
    let selected = schema::SenderSelection::decode(&prepared.sender).unwrap();
    let inventory = AdminInventory::new().unwrap();
    let pool = inventory._receiver.host_bytes();
    let original_idle = inventory._receiver.idle_available_bytes();
    let _original_work = inventory._receiver.host_work();
    let ((backing, retained), observed) = allocations_only(|| {
        let encoded =
            encode_selected(&prepared.base, &selected, &identity, &inventory.allowance).unwrap();
        let backing = encoded._payload.capacity() + encoded.signed.capacity();
        let live = snapshot();
        assert_eq!(
            live.allocations, 2,
            "only exact wrapper and signed output allocate"
        );
        assert_eq!(live.live, backing);
        assert_eq!(encoded._credit.bytes(), backing + 48);
        // This is the real keeper callback on the original ingress pool. The
        // two live Vecs and their stage-name reserve stay withheld after seal.
        inventory.allowance.finish_turn();
        let retained = original_idle - pool.available_bytes();
        assert!(retained >= backing + 48);
        drop(encoded);
        assert_eq!(
            snapshot().live,
            0,
            "backing is freed before its reservation"
        );
        assert!(original_idle - pool.available_bytes() < retained);
        (backing, retained)
    });
    assert_eq!(observed.live, 0);
    assert_eq!(observed.allocations, 2);
    assert!(!observed.unmatched_free);
    drop(inventory);
    assert_eq!(pool.available_bytes(), original_idle);
    eprintln!(
        "native admin measured W+S={backing}, sealed original-pool retention={retained}, inline Encoded={}, AdminError={}, CliError={}",
        std::mem::size_of::<Encoded>(),
        std::mem::size_of::<AdminError>(),
        std::mem::size_of::<CliError>()
    );
}

#[test]
fn sender_admin_actual_cli_error_transport_and_pressure_allocate_nothing() {
    let container = fixture();
    let root = container.path().join("owner");
    unselected(&root);
    let prepared = interrupt_prepared(&root, |_| {});
    let inventory = AdminInventory::new().unwrap();
    let pool = inventory._receiver.host_bytes();
    let original_idle = inventory._receiver.idle_available_bytes();
    let _original_work = inventory._receiver.host_work();
    let operation = Operation::existing(&root, inventory).unwrap();
    let selected = schema::SenderSelection::decode(&prepared.sender).unwrap();
    let pressure = operation
        .inventory
        .allowance
        .reserve(operation.inventory.allowance.available_bytes())
        .unwrap();
    let (cli, observed) = allocations_only(|| {
        let cause = match encode_selected(
            &prepared.base,
            &selected,
            &operation.identity,
            &operation.inventory.allowance,
        ) {
            Err(cause) => cause,
            Ok(_) => panic!("full original allowance must refuse before allocation"),
        };
        assert!(matches!(cause, Cause::Resource(_)));
        // This is the same concrete From and Debug used by main's Result
        // termination, not a test-only adapter or an anyhow error box.
        let cli = CliError::from(AdminError {
            cause,
            operation: Some(operation),
        });
        use std::io::Write;
        write!(&mut std::io::sink(), "{cli:?}").unwrap();
        cli
    });
    assert_no_new_backing(observed);
    drop(pressure);
    let CliError::Administration(error) = &cli else {
        panic!("actual native error must remain inline")
    };
    error
        .operation
        .as_ref()
        .unwrap()
        .inventory
        .allowance
        .finish_turn();
    assert!(
        pool.available_bytes() < original_idle,
        "original keeper is retained by error"
    );
    assert!(
        crate::store::SessionStore::open(&root).is_err(),
        "by-value CLI error retains original owner custody"
    );
    drop(cli);
    assert_eq!(pool.available_bytes(), original_idle);
    drop(crate::store::SessionStore::open(&root).unwrap());
}

#[test]
fn sender_admin_owned_final_check_keeps_actual_child_error_and_original_credit() {
    let container = fixture();
    let root = container.path().join("owner");
    initialize(&root).unwrap();
    let inventory = AdminInventory::new().unwrap();
    let pool = inventory._receiver.host_bytes();
    let original_idle = inventory._receiver.idle_available_bytes();
    let _original_work = inventory._receiver.host_work();
    let mut operation = Operation::existing(&root, inventory).unwrap();
    operation.run(Intent::Status).unwrap();
    let child = root.join("private-actions-v1");
    let moved = root.join("retained-child");
    std::fs::rename(&child, &moved).unwrap();
    std::fs::create_dir(&child).unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The ordinary Store check is not reclassified as paid work; the actual
    // immediately following production helper is observed independently.
    drop(operation.store.administrative_directory().unwrap());
    operation.inventory.allowance.finish_turn();
    let retained = original_idle - pool.available_bytes();
    let (cli, observed) = allocations_only(|| {
        let cause = operation.current_physical().unwrap_err();
        let cli = CliError::from(AdminError {
            cause,
            operation: Some(operation),
        });
        use std::io::Write;
        write!(&mut std::io::sink(), "{cli:?}").unwrap();
        cli
    });
    assert_no_new_backing(observed);
    assert_eq!(original_idle - pool.available_bytes(), retained);
    let CliError::Administration(error) = &cli else {
        panic!("wrong CLI error")
    };
    let source = std::error::Error::source(error)
        .unwrap()
        .downcast_ref::<std::io::Error>()
        .unwrap();
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.operation.as_ref().unwrap().physical_failure.is_some());
    let bounded = BoundedOwnerDirectory::admit(StorageDirectory::open(&root).unwrap()).unwrap();
    use atomic_file::lease::{AcquireError, FileLease, LeaseMode};
    assert!(matches!(
        FileLease::try_acquire(
            bounded
                .open_lease_existing(OsStr::new("private-actions-v1.writer.lock"))
                .unwrap(),
            LeaseMode::Exclusive
        ),
        Err(AcquireError::Busy)
    ));
    assert!(crate::store::SessionStore::open(&root).is_err());
    drop(cli);
    assert_eq!(pool.available_bytes(), original_idle);
    drop(
        FileLease::try_acquire(
            bounded
                .open_lease_existing(OsStr::new("private-actions-v1.writer.lock"))
                .unwrap(),
            LeaseMode::Exclusive,
        )
        .unwrap(),
    );
    std::fs::remove_dir(&child).unwrap();
    std::fs::rename(&moved, &child).unwrap();
    assert_eq!(status(&root).unwrap().phase, AdminPhase::Active);
}
