use super::*;
use crate::committed_authority::CommittedAuthority;
#[test]
fn staged_owner_turn_reserves_actual_single_large_decode_until_drop() {
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let inventory = ingress.host_work();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let available = pool.available_bytes();
    assert!(
        owner
            .prepare_data_turn(
                &pool,
                crate::owner::resources::OwnerDataOperation::StateStaged { input_len: 1 }
            )
            .is_err(),
        "generic turn must not bypass single-large-decode custody"
    );
    let mut turn = owner
        .prepare_staged_turn(&pool, &inventory, 2 * 1024 * 1024)
        .expect("a real staged turn must reserve the original pool and single decode lease");
    assert!(pool.available_bytes() < available);
    assert!(inventory.try_large_decode().is_err());
    let view = current.read().unwrap();
    owner
        .data_source(&mut turn, view.view(), &session, tests::now_ns())
        .unwrap();
    drop(view);
    drop(turn);
    assert_eq!(pool.available_bytes(), available);
    assert!(inventory.try_large_decode().is_ok());
    let (other, _rx) = crate::https::ingress_channel();
    assert!(
        owner
            .prepare_staged_turn(&pool, &other.host_work(), 1)
            .is_err()
    );
    assert!(owner.prepare_staged_turn(&pool, &inventory, 0).is_err());
    assert!(
        owner
            .prepare_staged_turn(&pool, &inventory, 64 * 1024 * 1024 + 1)
            .is_err()
    );
    let held = inventory.try_large_decode().unwrap();
    assert!(owner.prepare_staged_turn(&pool, &inventory, 1).is_err());
    drop(held);
    assert_eq!(pool.available_bytes(), available);
}

fn staged_bytes(owner: &SessionOwner, large: bool) -> Vec<u8> {
    use ui::automerge::{ROOT, transaction::Transactable};
    let mut author = owner.peer.clone();
    let mut doc =
        ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
    let heads = doc.get_heads();
    let mut value = vec![0; if large { 1152 * 1024 } else { 1024 }];
    blake3::Hasher::new()
        .update(b"actual staged native envelope")
        .finalize_xof()
        .fill(&mut value);
    doc.put(
        ROOT,
        "staged-native",
        ui::automerge::ScalarValue::Bytes(value),
    )
    .unwrap();
    doc.commit();
    let message = author
        .publish_incremental(
            "so.tilo.chat",
            "",
            doc.save_after(&heads),
            &owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}
fn stage(
    owner: &SessionOwner,
    ingress: &crate::https::Ingress,
    id: [u8; 16],
    bytes: &[u8],
) -> crate::delivery_http::upload::CompletedUpload {
    crate::delivery_http::upload::stage_for_owner_test(
        &owner.store,
        &ingress.host_work(),
        &ingress.host_bytes(),
        crate::staging_manifest::StageIdentity {
            workspace: owner.workspace_id,
            device: owner.peer.authority().device_public_key,
            delivery: id,
            digest: *blake3::hash(bytes).as_bytes(),
            total: bytes.len() as u64,
        },
        bytes,
    )
}
#[test]
fn staged_owner_large_native_admission_preserves_original_receipt_after_reopen() {
    let (_temp, mut owner) = tests::hosted_owner();
    let bytes = staged_bytes(&owner, true);
    assert!(
        bytes.len() > MAX_DELIVERY_ITEM_BYTES,
        "real native envelope must exceed inline delivery cap"
    );
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let complete = stage(&owner, &ingress, [221; 16], &bytes);
    let before = owner.committed_generation();
    let mut turn = owner
        .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
        .unwrap();
    let (outcome, reservation) = current
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_admit_staged(&mut turn, &source, complete, SystemTime::now())
        })
        .unwrap()
        .expect("complete real file must enter native admission under one signed barrier");
    let AdmissionOutcome::Durable(original) = outcome else {
        panic!("real upload not durable")
    };
    assert_eq!(
        original.result,
        tilo_relay::delivery_v3::commit::DurableAdmissionKind::Accepted
    );
    assert_eq!(original.committed.sequence, before + 1);
    assert_eq!(current.read().unwrap().generation(), before + 1);
    assert!(
        !owner.delivery_ledger().unwrap().replay[&(before + 1)]
            .publications
            .is_empty()
    );
    assert!(
        ingress.host_work().try_large_decode().is_err(),
        "lease stays held through Current publication"
    );
    drop(turn);
    drop(reservation); // persistent body is retained for an actual lost final reply.
    let root = owner.root().to_owned();
    let disk = std::fs::read(root.join("session.bin")).unwrap();
    drop(current);
    drop(owner);
    let mut owner = SessionOwner::open(&root).unwrap();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let complete = stage(&owner, &ingress, [221; 16], &bytes);
    let mut turn = owner
        .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
        .unwrap();
    let (retry, reservation) = current
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_admit_staged(&mut turn, &source, complete, SystemTime::now())
        })
        .unwrap()
        .unwrap();
    let AdmissionOutcome::Durable(retry) = retry else {
        panic!("missing original retry")
    };
    assert_eq!(retry, original);
    assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), disk);
    drop(turn);
    reservation.remove().unwrap();
}

#[test]
fn staged_owner_rechecks_actual_file_source_root_inventory_and_expiry_before_admission() {
    use error::OwnerDataFailureKind as Kind;
    use std::io::Write;
    for case in 0..8 {
        let (_temp, mut owner) = tests::hosted_owner();
        let bytes = staged_bytes(&owner, false);
        let (ingress, _rx) = crate::https::ingress_channel();
        let pool = ingress.host_bytes();
        owner.attach_data_resources(&pool).unwrap();
        let current = CommittedAuthority::new(&owner);
        let session = tests::authenticated_session(&owner, &current);
        let other_root = tempfile::tempdir().unwrap();
        let other_store =
            crate::store::SessionStore::create(&other_root.path().join("other")).unwrap();
        let separate_inventory = crate::store::HostWorkInventory::new(&pool).unwrap();
        let identity = crate::staging_manifest::StageIdentity {
            workspace: owner.workspace_id,
            device: if case == 3 {
                [19; 32]
            } else {
                owner.peer.authority().device_public_key
            },
            delivery: [222; 16],
            digest: *blake3::hash(&bytes).as_bytes(),
            total: bytes.len() as u64,
        };
        let (foreign_ingress, _foreign_rx) = crate::https::ingress_channel();
        let staging_pool = if case == 7 {
            foreign_ingress.host_bytes()
        } else {
            pool.clone()
        };
        let inventory = if case == 4 {
            separate_inventory
        } else if case == 7 {
            foreign_ingress.host_work()
        } else {
            ingress.host_work()
        };
        let store = if case == 2 {
            &other_store
        } else {
            &owner.store
        };
        let complete = crate::delivery_http::upload::stage_for_owner_test(
            store,
            &inventory,
            &staging_pool,
            identity,
            &bytes,
        );
        let data = std::fs::read_dir(store.root().join("staging-v1"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "data"))
            .unwrap();
        if case == 0 {
            let mut file = std::fs::OpenOptions::new().write(true).open(&data).unwrap();
            file.write_all(&[bytes[0] ^ 1]).unwrap();
            file.sync_all().unwrap();
        } else if case == 1 {
            let path = data.with_extension("meta");
            let mut manifest =
                crate::staging_manifest::StageManifest::decode(&std::fs::read(&path).unwrap())
                    .unwrap();
            manifest.expires_at_ns = 1;
            std::fs::write(path, manifest.encode().unwrap()).unwrap();
        } else if case == 5 {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::rename(&data, other_root.path().join("original-file")).unwrap();
            let mut replacement = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&data)
                .unwrap();
            replacement.write_all(&bytes).unwrap();
            replacement.sync_all().unwrap();
        }
        let before = owner.committed_generation();
        let disk = std::fs::read(owner.root().join("session.bin")).unwrap();
        let mut turn = if case == 6 {
            owner
                .prepare_data_turn(
                    &pool,
                    crate::owner::resources::OwnerDataOperation::State {
                        input_len: bytes.len(),
                    },
                )
                .unwrap()
        } else {
            owner
                .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
                .unwrap()
        };
        let result = current
            .with_owner_view(&mut owner, |view, owner| {
                let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
                owner.data_admit_staged(&mut turn, &source, complete, SystemTime::now())
            })
            .unwrap();
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("staged invalid custody case {case} admitted"),
        };
        assert_eq!(
            error.kind(),
            if case == 0 {
                Kind::InvalidNativeInput
            } else {
                Kind::CurrentDenied
            },
            "case {case}"
        );
        assert_eq!(owner.committed_generation(), before);
        assert_eq!(
            std::fs::read(owner.root().join("session.bin")).unwrap(),
            disk
        );
        assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
        assert!(
            data.exists(),
            "failure must retain inert stage and disk quota"
        );
    }
}

#[test]
fn staged_owner_real_barrier_failure_returns_no_receipt_and_keeps_staged_file() {
    use crate::store::test_fault::{Point, with_fault};
    let (_temp, mut owner) = tests::hosted_owner();
    let bytes = staged_bytes(&owner, false);
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let complete = stage(&owner, &ingress, [223; 16], &bytes);
    let root = owner.root().to_owned();
    let before = owner.committed_generation();
    let mut turn = owner
        .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
        .unwrap();
    let mut callback_kind = None;
    let result = with_fault(&root, Point::DirectoryBarrier, || {
        current.with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            let result = owner.data_admit_staged(&mut turn, &source, complete, SystemTime::now());
            callback_kind = result.as_ref().err().map(|error| error.kind());
            result
        })
    });
    assert!(
        result.is_err(),
        "uncertain owner must fail Current publication"
    );
    assert_eq!(
        callback_kind,
        Some(error::OwnerDataFailureKind::DurabilityUncertain)
    );
    assert_eq!(owner.committed_generation(), before);
    assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
    assert!(current.read().is_err());
    assert_eq!(
        std::fs::read_dir(root.join("staging-v1")).unwrap().count(),
        2
    );
    assert!(
        owner
            .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
            .is_err()
    );
    drop(turn);
    assert!(ingress.host_work().try_large_decode().is_ok());
}

#[test]
fn staged_owner_existing_delivery_id_conflict_is_typed_before_native_decode() {
    let (_temp, mut owner) = tests::hosted_owner();
    let mut bytes = staged_bytes(&owner, false);
    let delivery_id = [224; 16];
    owner
        .receive_delivery(
            TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
            CompleteStateItem {
                delivery_id,
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    bytes[0] ^= 1;
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    owner.attach_data_resources(&pool).unwrap();
    let current = CommittedAuthority::new(&owner);
    let session = tests::authenticated_session(&owner, &current);
    let complete = stage(&owner, &ingress, delivery_id, &bytes);
    let generation = owner.committed_generation();
    let disk = std::fs::read(owner.root().join("session.bin")).unwrap();
    let mut turn = owner
        .prepare_staged_turn(&pool, &ingress.host_work(), bytes.len())
        .unwrap();
    let result = current
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &session, tests::now_ns())?;
            owner.data_admit_staged(&mut turn, &source, complete, SystemTime::now())
        })
        .unwrap();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("conflicting canonical body admitted"),
    };
    assert_eq!(error.kind(), error::OwnerDataFailureKind::ExactIdConflict);
    assert_eq!(owner.committed_generation(), generation);
    assert_eq!(owner.delivery_ledger().unwrap().admissions.len(), 1);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        disk
    );
}
