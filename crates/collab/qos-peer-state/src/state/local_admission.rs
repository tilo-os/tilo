//! Local exact retries use the signed author, never an invented carrier origin.

use super::*;

impl StatePeer {
    /// Look up an already recorded local envelope with current authority,
    /// signature and route validation. The caller must independently require
    /// that its selected local device is exactly `envelope.author`.
    pub fn lookup_exact_local_document(
        &self,
        app: &str,
        record: &str,
        envelope: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        self.lookup_exact_document_parts(
            *envelope.author.as_bytes(),
            app,
            &self.core.room,
            record,
            envelope,
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted() -> (StatePeer, SignedOp, SigningKey, DeviceCert, SystemTime) {
        let (mut peer, key, other, certificate, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = peer
            .publish_document(
                "test.app",
                "tab:1",
                peer.document("test.app", "tab:1").unwrap(),
                &key,
                now,
            )
            .unwrap()
        else {
            panic!("expected actual signed document publication")
        };
        (peer, *envelope, other, certificate, now)
    }

    #[test]
    fn local_exact_retry_recognizes_recorded_envelope_without_mutation_or_freshness_reset() {
        let (peer, envelope, _, _, now) = admitted();
        let expected =
            DocumentAdmission::from_op(peer.room(), "test.app", "tab:1", &envelope).unwrap();
        let before = peer.durable().unwrap();
        for time in [now, now + Duration::from_secs(3600)] {
            assert_eq!(
                peer.lookup_exact_local_document("test.app", "tab:1", &envelope, time)
                    .unwrap(),
                Some(expected.clone())
            );
        }
        assert_eq!(peer.durable().unwrap(), before);
    }

    #[test]
    fn local_exact_retry_rejects_wrong_route_and_tampered_identity() {
        let (peer, envelope, _, other, now) = admitted();
        assert!(
            peer.lookup_exact_local_document("other.app", "tab:1", &envelope, now)
                .is_err()
        );
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:2", &envelope, now)
                .is_err()
        );
        let mut tampered = envelope.clone();
        tampered.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &tampered, now)
                .is_err()
        );
    }

    #[test]
    fn local_exact_retry_does_not_reuse_another_valid_signers_evidence() {
        let (peer, mut envelope, key, other, now) = admitted();
        envelope.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        collab_types::sign_op(&mut envelope, &key).unwrap();
        assert_eq!(
            peer.lookup_exact_local_document("test.app", "tab:1", &envelope, now)
                .unwrap(),
            None
        );
    }
}
