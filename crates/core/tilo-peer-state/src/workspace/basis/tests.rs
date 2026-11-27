use super::*;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::RevocationReason;
fn fixture() -> (AutoCommit, SigningKey, String) {
    let root = SigningKey::from_bytes(&[91; 32]);
    let device = SigningKey::from_bytes(&[92; 32]);
    let cert = crate::fixture::cert(
        &root,
        &device,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let changes = crate::state::seed_workspace(
        &uuid::Uuid::from_bytes([1; 16]).to_string(),
        &cert,
        std::slice::from_ref(&cert),
    )
    .unwrap();
    (
        crate::workspace::candidate(None, &changes).unwrap(),
        root,
        hex::encode(cert.device_public_key),
    )
}
fn revocation(root: &SigningKey, time: u64) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: [99; 32],
        revoked_at_ns: time,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
    entry
}
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts the two readings agree before returning the basis"
)]
fn basis(doc: &AutoCommit, extra: &[RevocationEntry]) -> Result<[u8; 32], BasisError> {
    let basis = of_document(doc, extra);
    // A governance state reading its once-decoded rows has the same basis.
    let state = crate::cached_document::CachedDocument::from(doc.clone());
    assert_eq!(super::decoded(&state, extra), basis);
    basis
}
tilo_test_env::scenario! {
    fn media_basis_reads_the_signed_workspace_as_the_original_did;

    phase media_basis_matches_original_signed_workspace;
    {
        let (doc, _, _) = fixture();
        let expected = historical_basis(&doc, &[]).unwrap();
        assert_eq!(basis(&doc, &[]), Ok(expected));
    }

    phase media_basis_preserves_verified_earliest_revocation_and_canonical_tie_order;
    {
        let (mut doc, root, device) = fixture();
        let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
        let original = super::strict_bytes(&doc, &identities, &device).unwrap();
        let mut row = crate::state::DeviceRow::decode(&original).unwrap();
        row.revocations = vec![revocation(&root, 20), revocation(&root, 10)];
        doc.put(
            &identities,
            &device,
            ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
        doc.commit();
        let mut tie = revocation(&root, 10);
        tie.reason = RevocationReason::Lost;
        tilo_user_identity::sign_revocation_entry(&mut tie, &root).unwrap();
        for extra in [
            vec![],
            vec![tie.clone(), revocation(&root, 5)],
            vec![revocation(&root, 5), tie],
        ] {
            let expected = historical_basis(&doc, &extra).unwrap();
            assert_eq!(basis(&doc, &extra), Ok(expected));
        }
        let mut invalid = revocation(&root, 1);
        invalid.signature[0] ^= 1;
        assert_eq!(basis(&doc, &[invalid]), Err(BasisError::Revocation));
    }

    phase media_basis_matches_original_malformed_container_conflict_and_certificate_rejections;
    {
        for mode in 0..6 {
            let (mut doc, _, device) = fixture();
            match mode {
                0 => {
                    doc.put(ROOT, "owner", "wrong key").unwrap();
                }
                1 => {
                    doc.put_object(ROOT, "peers", ObjType::List).unwrap();
                }
                2 => {
                    doc.put(ROOT, "peers", "wrong shape").unwrap();
                }
                3 => {
                    let ids = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
                    doc.put(&ids, &device, ScalarValue::Bytes(vec![0xff]))
                        .unwrap();
                }
                4 => {
                    let mut fork = doc.fork();
                    doc.put(ROOT, "owner", "first").unwrap();
                    doc.commit();
                    fork.put(ROOT, "owner", "second").unwrap();
                    fork.commit();
                    doc.merge_incoming(&mut fork).unwrap();
                }
                _ => {
                    let ids = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
                    let bytes = super::strict_bytes(&doc, &ids, &device).unwrap();
                    let mut row = crate::state::DeviceRow::decode(&bytes).unwrap();
                    row.device_cert.signature[0] ^= 1;
                    doc.put(&ids, &device, ScalarValue::Bytes(row.encode().unwrap()))
                        .unwrap();
                }
            }
            doc.commit();
            let expected = historical_basis(&doc, &[]).unwrap_err();
            assert_eq!(basis(&doc, &[]), Err(expected));
        }
    }

    phase media_basis_accounts_escaped_labels_duplicate_proofs_and_revocation_map_growth;
    {
        let (mut doc, root, device) = fixture();
        let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
        let bytes = super::strict_bytes(&doc, &identities, &device).unwrap();
        let mut row = crate::state::DeviceRow::decode(&bytes).unwrap();
        row.device_cert.device_label = "escaped \" \\ \n".repeat(1024);
        tilo_user_identity::sign_device_cert(&mut row.device_cert, &root).unwrap();
        let proof = crate::state::RedemptionReceipt {
            invite_id: [1; 32],
            user_public_key: row.device_cert.user_public_key,
            device_public_key: row.device_cert.device_public_key,
            request_id: [2; 32],
            role: crate::join::Role::Editor,
        };
        row.admission = Some(proof.clone());
        row.admissions = vec![proof.clone(), proof];
        for id in 1..=32 {
            let mut entry = revocation(&root, u64::from(id));
            entry.revoked_device_public_key = [id; 32];
            tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
            row.revocations.push(entry);
        }
        doc.put(
            &identities,
            &device,
            ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
        doc.commit();
        let expected = historical_basis(&doc, &[]).unwrap();
        assert_eq!(basis(&doc, &[]), Ok(expected));
        // Existing admission binding validation remains independent of byte budget.
        row.admissions[0].user_public_key = [7; 32];
        doc.put(
            &identities,
            &device,
            ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
        doc.commit();
        assert_eq!(historical_basis(&doc, &[]), Err(BasisError::Identity));
        assert_eq!(basis(&doc, &[]), Err(BasisError::Identity));
    }

    phase media_basis_refuses_a_revocation_from_another_root;
    {
        let (doc, _, _) = fixture();
        let foreign = SigningKey::from_bytes(&[44; 32]);
        let extra = revocation(&foreign, 1);
        assert_eq!(
            historical_basis(&doc, std::slice::from_ref(&extra)),
            Err(BasisError::Revocation)
        );
        assert_eq!(basis(&doc, &[extra]), Err(BasisError::Revocation));
    }
}

// Frozen original algorithm is the independent byte/error oracle.
use super::{canonical_key, strict_bytes, strict_map, strict_string};
use std::collections::{BTreeMap, BTreeSet};
use tilo_user_identity::canonical::canonical_bytes;
#[allow(
    clippy::too_many_lines,
    reason = "the frozen oracle stays one readable copy of the original algorithm"
)]
fn historical_basis(
    doc: &AutoCommit,
    extra_revocations: &[tilo_user_identity::RevocationEntry],
) -> Result<[u8; 32], BasisError> {
    let owner = strict_string(doc, &ROOT, "owner")?;
    let owner_key = canonical_key(&owner)?;
    let membership = crate::members::replay_declared(doc);
    let mut grants = BTreeMap::new();
    for grant in &membership.grants {
        grants.insert(
            grant.id.as_str().to_owned(),
            canonical_bytes(grant).map_err(|_| BasisError::Malformed)?,
        );
    }
    let members: BTreeMap<String, tilo_action_policy::Role> = membership
        .members
        .iter()
        .map(|(person, assigned)| (person.clone(), assigned.role()))
        .collect();
    let peers = strict_map(doc, &ROOT, "peers")?;
    let mut roster = BTreeSet::new();
    for key in doc.keys(&peers) {
        strict_string(doc, &peers, &key)?;
        roster.insert(canonical_key(&key)?);
    }
    let identities = strict_map(doc, &ROOT, "peer_identities_v1")?;
    let mut rows = BTreeMap::new();
    let mut certificates = BTreeMap::new();
    let mut revocations = Vec::new();
    for key in doc.keys(&identities) {
        let bytes = strict_bytes(doc, &identities, &key)?;
        let row = crate::state::DeviceRow::decode(&bytes).map_err(|_| BasisError::Malformed)?;
        let device = canonical_key(&key)?;
        if device != row.device_cert.device_public_key {
            return Err(BasisError::Identity);
        }
        tilo_user_identity::verify_device_cert(&row.device_cert)
            .map_err(|_| BasisError::Identity)?;
        let mut admissions = BTreeSet::new();
        for proof in row.admission.iter().chain(&row.admissions) {
            if proof.device_public_key != device
                || proof.user_public_key != row.device_cert.user_public_key
            {
                return Err(BasisError::Identity);
            }
            admissions.insert(canonical_bytes(proof).map_err(|_| BasisError::Malformed)?);
        }
        let mut delegations = BTreeSet::new();
        for delegation in &row.workspace_delegations {
            delegations.insert(canonical_bytes(delegation).map_err(|_| BasisError::Malformed)?);
        }
        for entry in row.revocations {
            if entry.user_public_key != row.device_cert.user_public_key {
                return Err(BasisError::Revocation);
            }
            revocations.push(entry);
        }
        certificates.insert(device, row.device_cert.user_public_key);
        rows.insert(device, (row.device_cert, admissions, delegations));
    }
    if !certificates.values().any(|root| *root == owner_key) {
        return Err(BasisError::OwnerUnadmitted);
    }
    if let Some(device) = roster
        .iter()
        .find(|device| !certificates.contains_key(*device))
    {
        return Err(BasisError::NamesNobody(*device));
    }
    revocations.extend_from_slice(extra_revocations);
    // RevocationList keeps the earliest entry. Sorting canonical encodings makes
    // equal timestamps deterministic even if duplicate row order differs.
    let mut encoded = BTreeMap::new();
    for entry in revocations {
        if !certificates
            .values()
            .any(|root| *root == entry.user_public_key)
            || certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|root| *root != entry.user_public_key)
        {
            return Err(BasisError::Revocation);
        }
        encoded.insert(
            canonical_bytes(&entry).map_err(|_| BasisError::Malformed)?,
            entry,
        );
    }
    let mut verified = tilo_user_identity::RevocationList::new();
    for entry in encoded.into_values() {
        verified.insert(entry).map_err(|_| BasisError::Revocation)?;
    }
    let revocations = verified
        .iter()
        .map(|entry| (entry.revoked_device_public_key, entry))
        .collect::<BTreeMap<_, _>>();
    let bytes = canonical_bytes(&(owner_key, members, grants, roster, rows, revocations))
        .map_err(|_| BasisError::Malformed)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-workspace-media-auth-basis-v1\0");
    hasher.update(&bytes);
    Ok(*hasher.finalize().as_bytes())
}
