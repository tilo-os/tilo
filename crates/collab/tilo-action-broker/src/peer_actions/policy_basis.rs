//! Pure policy data, never owner provenance, authorization, or a readiness witness.
pub use tilo_peer_state::owner_commit::PrivatePolicyBasis;
#[cfg(test)]
mod tests {
    #![allow(
        clippy::similar_names,
        clippy::arithmetic_side_effects,
        reason = "fixture keys pair root/room names; epoch offsets are small constants"
    )]
    use super::*;
    use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    use ed25519_dalek::SigningKey;
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, Subject, TreePath};
    use tilo_automerge::ApplyIncoming;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    fn peer(blobs: &[Vec<u8>], title: &str) -> tilo_peer_state::StatePeer {
        let root = SigningKey::from_bytes(&[21; 32]);
        let key = SigningKey::from_bytes(&[22; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "policy".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let room = uuid::Uuid::from_u128(33).to_string();
        let roster = vec![cert.clone()];
        let changes = tilo_peer_state::seed_room(&room, &cert, &roster).unwrap();
        let mut doc = AutoCommit::new();
        for bytes in changes {
            doc.apply_incoming([automerge::Change::from_bytes(bytes).unwrap()])
                .unwrap();
        }
        let caps = doc.get(ROOT, "grants").unwrap().unwrap().1;
        for (index, blob) in blobs.iter().enumerate() {
            doc.put(&caps, index.to_string(), ScalarValue::Bytes(blob.clone()))
                .unwrap();
        }
        doc.put(ROOT, "name", title).unwrap();
        doc.commit();
        let mut peer = tilo_peer_state::StatePeer::new(
            &room,
            cert,
            roster,
            RevocationList::new(),
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
        )
        .unwrap();
        peer.install_seed(
            tilo_peer_state::SeedState {
                room_changes: doc
                    .get_changes(&[])
                    .into_iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect(),
                documents: vec![],
            },
            &key,
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
        )
        .unwrap();
        peer
    }
    fn cap(lifetime: Lifetime) -> Vec<u8> {
        serde_json::to_vec(
            &Grant::new(
                "editors-read",
                Subject::Role(Role::Editor),
                TreePath::room(uuid::Uuid::from_u128(33).to_string()),
                KindSet::READ,
                Answer::Allow,
                Subject::person("owner"),
            )
            .with_lifetime(lifetime),
        )
        .unwrap()
    }
    #[test]
    fn private_policy_basis_matches_existing_domain_and_ordered_time_boundaries() {
        let values = [
            cap(Lifetime::Until(8_000)),
            cap(Lifetime::Until(7_000)),
            cap(Lifetime::Until(7_000)),
            cap(Lifetime::Until(3_000)),
            cap(Lifetime::UntilRevoked),
        ];
        let a = peer(&values, "A");
        let b = peer(&values, "B");
        let basis = PrivatePolicyBasis::from_peer(&a).unwrap();
        assert_eq!(
            basis.revision(),
            tilo_peer_state::owner_commit::governance_revision(&a).unwrap()
        );
        assert_eq!(basis, PrivatePolicyBasis::from_peer(&b).unwrap());
        assert_eq!(basis.next_expiry_after(0), Some(3));
        assert_eq!(basis.next_expiry_after(3), Some(7));
        assert_eq!(basis.next_expiry_after(7), Some(8));
        assert_eq!(basis.next_expiry_after(8), None);
        assert_ne!(
            basis,
            PrivatePolicyBasis::from_peer(&peer(&[cap(Lifetime::Until(9_000))], "A")).unwrap()
        );
    }
    #[test]
    fn private_policy_basis_fails_closed_for_unparsed_or_nonpersistent_grants() {
        for blob in [
            b"not-json".to_vec(),
            cap(Lifetime::Session("session".into())),
            cap(Lifetime::Once),
        ] {
            assert!(PrivatePolicyBasis::from_peer(&peer(&[blob], "A")).is_err());
        }
    }
    #[test]
    fn private_policy_basis_preserves_missing_room_and_general_revision_scope() {
        let source = peer(&[cap(Lifetime::Once)], "policy");
        assert_eq!(
            PrivatePolicyBasis::from_peer(&source).unwrap_err(),
            "nonpersistent governed private policy grant"
        );
        assert!(tilo_peer_state::owner_commit::canonical_owner(&source).is_ok());
        assert!(
            tilo_peer_state::owner_commit::governance_revision(&source).is_ok(),
            "general revision is not the strict private persistence gate"
        );
        let empty = tilo_peer_state::StatePeer::new(
            source.room(),
            source.authority().clone(),
            source.certificates(),
            RevocationList::new(),
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            PrivatePolicyBasis::from_peer(&empty).unwrap_err(),
            "private policy room unavailable"
        );
        assert_eq!(
            tilo_peer_state::owner_commit::canonical_owner(&empty).unwrap_err(),
            "admitted room missing"
        );
        assert_eq!(
            tilo_peer_state::owner_commit::governance_revision(&empty).unwrap_err(),
            "admitted room missing"
        );
    }
}
