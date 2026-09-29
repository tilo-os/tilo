use super::*;
use crate::resources::native::NativeBatch;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationReason};
fn fixture() -> (AutoCommit, SigningKey, String) {
    let root = SigningKey::from_bytes(&[91; 32]);
    let device = SigningKey::from_bytes(&[92; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Governor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let changes = crate::state::seed_room(
        &uuid::Uuid::from_bytes([1; 16]).to_string(),
        &cert,
        std::slice::from_ref(&cert),
    )
    .unwrap();
    (
        crate::room::candidate(None, &changes).unwrap(),
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
fn native(
    doc: &mut AutoCommit,
    extra: &[RevocationEntry],
    collection_bytes: usize,
) -> Result<[u8; 32], Error> {
    let doc = NativeBatch::new(8 * 1024 * 1024).load(&doc.save()).unwrap();
    bounded(
        &doc.read(),
        extra,
        BasisLimits {
            collection_bytes,
            json: JsonLimits {
                allocation_bytes: 2 * 1024 * 1024,
                collection_entries: 4096,
            },
        },
    )
}
#[test]
fn media_basis_matches_original_signed_room() {
    let (mut doc, _, _) = fixture();
    let expected = historical_basis(&doc, &[]).unwrap();
    assert_eq!(native(&mut doc, &[], 4 * 1024 * 1024), Ok(expected));
    assert_eq!(
        super::super::parse(&doc),
        Ok(None),
        "a basis projection never initializes policy"
    );
}
#[test]
fn media_basis_preserves_verified_earliest_revocation_and_canonical_tie_order() {
    let (mut doc, root, device) = fixture();
    let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let original = super::super::strict_bytes(&doc, &identities, &device).unwrap();
    let mut row: crate::state::GovernedIdentity = serde_json::from_slice(&original).unwrap();
    row.revocations = vec![revocation(&root, 20), revocation(&root, 10)];
    doc.put(
        &identities,
        &device,
        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
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
        assert_eq!(native(&mut doc, &extra, 4 * 1024 * 1024), Ok(expected));
    }
    let mut invalid = revocation(&root, 1);
    invalid.signature[0] ^= 1;
    assert_eq!(
        native(&mut doc, &[invalid], 4 * 1024 * 1024),
        Err(Error::Policy(MediaPolicyError::Binding))
    );
    assert_eq!(native(&mut doc, &[], 16), Err(Error::Capacity));
}
#[test]
fn media_basis_matches_original_malformed_container_conflict_and_certificate_rejections() {
    for mode in 0..6 {
        let (mut doc, _, device) = fixture();
        match mode {
            0 => {
                doc.put(ROOT, "owner", "wrong key").unwrap();
            }
            1 => {
                doc.put_object(ROOT, "capabilities", ObjType::List).unwrap();
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
                let bytes = super::super::strict_bytes(&doc, &ids, &device).unwrap();
                let mut row: crate::state::GovernedIdentity =
                    serde_json::from_slice(&bytes).unwrap();
                row.device_cert.signature[0] ^= 1;
                doc.put(
                    &ids,
                    &device,
                    ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
                )
                .unwrap();
            }
        }
        doc.commit();
        let expected = historical_basis(&doc, &[]).unwrap_err();
        assert_eq!(
            native(&mut doc, &[], 4 * 1024 * 1024),
            Err(Error::Policy(expected))
        );
    }
}
#[test]
fn media_basis_accounts_escaped_labels_duplicate_proofs_and_revocation_map_growth() {
    let (mut doc, root, device) = fixture();
    let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let bytes = super::super::strict_bytes(&doc, &identities, &device).unwrap();
    let mut row: crate::state::GovernedIdentity = serde_json::from_slice(&bytes).unwrap();
    row.device_cert.device_label = "escaped \" \\ \n".repeat(1024);
    tilo_user_identity::sign_device_cert(&mut row.device_cert, &root).unwrap();
    let proof = crate::state::RedemptionReceipt {
        invite_id: [1; 32],
        user_public_key: row.device_cert.user_public_key,
        device_public_key: row.device_cert.device_public_key,
        request_id: [2; 32],
        role: crate::join::MemberRole::Editor,
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
        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
    )
    .unwrap();
    doc.commit();
    let expected = historical_basis(&doc, &[]).unwrap();
    assert_eq!(native(&mut doc, &[], 4 * 1024 * 1024), Ok(expected));
    // Existing admission binding validation remains independent of byte budget.
    row.admissions[0].user_public_key = [7; 32];
    doc.put(
        &identities,
        &device,
        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
    )
    .unwrap();
    doc.commit();
    assert_eq!(historical_basis(&doc, &[]), Err(MediaPolicyError::Binding));
    assert_eq!(
        native(&mut doc, &[], 4 * 1024 * 1024),
        Err(Error::Policy(MediaPolicyError::Binding))
    );
}

#[test]
fn media_basis_collection_and_json_limits_refuse_without_mutating_original_document() {
    let (mut doc, _, _) = fixture();
    let original = doc.save();
    let native = NativeBatch::new(8 * 1024 * 1024).load(&original).unwrap();
    for (collection_bytes, json_bytes, refusal) in [
        (0, 2 * 1024 * 1024, Error::Capacity),
        (
            4 * 1024 * 1024,
            1,
            Error::Decode(PeerResourceError::Capacity),
        ),
    ] {
        let refused = bounded(
            &native.read(),
            &[],
            BasisLimits {
                collection_bytes,
                json: JsonLimits {
                    allocation_bytes: json_bytes,
                    collection_entries: 4096,
                },
            },
        );
        assert_eq!(refused, Err(refusal));
    }
    assert_eq!(doc.save(), original);
    let foreign = SigningKey::from_bytes(&[44; 32]);
    let extra = revocation(&foreign, 1);
    assert_eq!(
        historical_basis(&doc, std::slice::from_ref(&extra)),
        Err(MediaPolicyError::Binding)
    );
    assert_eq!(
        self::native(&mut doc, &[extra], 4 * 1024 * 1024),
        Err(Error::Policy(MediaPolicyError::Binding))
    );
}

// Frozen original algorithm is the independent byte/error oracle.
use super::super::{canonical_key, strict_bytes, strict_map, strict_string};
use std::collections::{BTreeMap, BTreeSet};
use tilo_user_identity::canonical::canonical_bytes;
fn historical_basis(
    doc: &AutoCommit,
    extra_revocations: &[tilo_user_identity::RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    let owner = strict_string(doc, &ROOT, "owner")?;
    let owner_key = canonical_key(&owner)?;
    let caps = strict_map(doc, &ROOT, "capabilities")?;
    let mut capabilities = BTreeMap::new();
    for key in doc.keys(&caps) {
        let bytes = strict_bytes(doc, &caps, &key)?;
        let value: collab_types::caps::CapOverride =
            serde_json::from_slice(&bytes).map_err(|_| MediaPolicyError::Malformed)?;
        capabilities.insert(
            key,
            canonical_bytes(&value).map_err(|_| MediaPolicyError::Malformed)?,
        );
    }
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
        let row: crate::state::GovernedIdentity =
            serde_json::from_slice(&bytes).map_err(|_| MediaPolicyError::Malformed)?;
        let device = canonical_key(&key)?;
        if device != row.device_cert.device_public_key {
            return Err(MediaPolicyError::Binding);
        }
        tilo_user_identity::verify_device_cert(&row.device_cert)
            .map_err(|_| MediaPolicyError::Binding)?;
        let mut admissions = BTreeSet::new();
        for proof in row.admission.iter().chain(&row.admissions) {
            if proof.device_public_key != device
                || proof.user_public_key != row.device_cert.user_public_key
            {
                return Err(MediaPolicyError::Binding);
            }
            admissions.insert(canonical_bytes(proof).map_err(|_| MediaPolicyError::Malformed)?);
        }
        let mut delegations = BTreeSet::new();
        for delegation in &row.workspace_delegations {
            delegations
                .insert(canonical_bytes(delegation).map_err(|_| MediaPolicyError::Malformed)?);
        }
        for entry in row.revocations {
            if entry.user_public_key != row.device_cert.user_public_key {
                return Err(MediaPolicyError::Binding);
            }
            revocations.push(entry);
        }
        certificates.insert(device, row.device_cert.user_public_key);
        rows.insert(device, (row.device_cert, admissions, delegations));
    }
    if !certificates.values().any(|root| *root == owner_key)
        || roster
            .iter()
            .any(|device| !certificates.contains_key(device))
    {
        return Err(MediaPolicyError::Binding);
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
            return Err(MediaPolicyError::Binding);
        }
        encoded.insert(
            canonical_bytes(&entry).map_err(|_| MediaPolicyError::Malformed)?,
            entry,
        );
    }
    let mut verified = tilo_user_identity::RevocationList::new();
    for entry in encoded.into_values() {
        verified
            .insert(entry)
            .map_err(|_| MediaPolicyError::Binding)?;
    }
    let revocations = verified
        .iter()
        .map(|entry| (entry.revoked_device_public_key, entry))
        .collect::<BTreeMap<_, _>>();
    let bytes = canonical_bytes(&(owner_key, capabilities, roster, rows, revocations))
        .map_err(|_| MediaPolicyError::Malformed)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-room-media-auth-basis-v1\0");
    hasher.update(&bytes);
    Ok(*hasher.finalize().as_bytes())
}
