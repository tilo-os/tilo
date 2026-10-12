//! Local exact retries use the signed author, never an invented carrier origin.

use super::{AdmissionError, DocumentAdmission, LocalWrite, SignedOp, StatePeer, SystemTime};

impl StatePeer {
    /// Look up an already recorded local envelope with current authority,
    /// signature and route validation. The caller must independently require
    /// that its selected local device is exactly `envelope.author`.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if the envelope fails authority, signature or route
    /// validation.
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

    /// Admit `envelope`, a local app's signed write to `record`, as a new
    /// exact admission: refused unless it is admitted now, completely and
    /// not as a duplicate, with evidence of it in the exact admission index.
    /// An owner admits into a candidate and discards it on an error.
    ///
    /// # Errors
    ///
    /// Returns a message when the write is refused, deferred, incomplete or
    /// rejected (`not newly admitted`), when its signed sequence is at or
    /// below the author's replay floor on the route (`signed sequence is
    /// stale`), or when it leaves no exact admission evidence. None of these
    /// waits for an asynchronous receipt: the evidence is the exact admission
    /// index entry written by this same synchronous call.
    pub fn admit_new_local_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        now: SystemTime,
    ) -> Result<(), String> {
        self.admit_new_local(app, record, envelope, LocalWrite::Plain, now)
    }

    /// [`Self::admit_new_local_document`] for a sealed write, by the owner
    /// that holds the channel's keys. The sealed envelope passes every
    /// check first (certificate, role, route, the author's signature over
    /// the ciphertext and its sequence); only then does `open` decrypt it,
    /// and the plaintext it returns takes the normal admission path. `open`
    /// must refuse anything it cannot open under the channel's current
    /// epoch. The record stays encrypted: a plaintext write to it is later
    /// refused as a downgrade.
    ///
    /// # Errors
    ///
    /// As [`Self::admit_new_local_document`], for a write that is not
    /// sealed, and when `open` refuses it.
    pub fn admit_new_opened_local_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        now: SystemTime,
    ) -> Result<(), String> {
        self.admit_new_local(app, record, envelope, LocalWrite::Opened(open), now)
    }

    fn admit_new_local(
        &mut self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        write: LocalWrite<'_>,
        now: SystemTime,
    ) -> Result<(), String> {
        let author = *envelope.author.as_bytes();
        let before = self.clone();
        let outcome = self.admit_local(app, record, envelope, write, now)?;
        if outcome.duplicate {
            return Err("native local document signed sequence is stale".into());
        }
        if outcome.deferred || outcome.incomplete.is_some() || outcome.rejected != 0 {
            return Err(
                "native local document was deferred, incomplete or rejected, not newly admitted"
                    .into(),
            );
        }
        let admissions = self.admissions_since(&before).map_err(|e| e.to_string())?;
        if !admissions.iter().any(|entry| {
            entry.room() == self.core.room
                && entry.app() == app
                && entry.record() == record
                && entry.author() == author
        }) {
            return Err("native local document has no exact admission evidence".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;
    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::DeviceCert;
    use web_time::Duration;

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
    fn a_new_local_write_is_admitted_once_with_exact_evidence() {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        let bytes = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = peer
            .clone()
            .publish_document("test.app", "tab:1", bytes, &key, now)
            .unwrap()
        else {
            panic!("expected actual signed document publication")
        };
        peer.admit_new_local_document("test.app", "tab:1", (*envelope).clone(), now)
            .unwrap();
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &envelope, now)
                .unwrap()
                .is_some()
        );
        assert!(
            peer.clone()
                .admit_new_local_document("test.app", "tab:1", *envelope, now)
                .is_err(),
            "an exact retry is not a new admission"
        );
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
        let mut tampered = envelope;
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

    #[test]
    fn replay_floor_without_exact_entry_requires_a_fresh_signed_successor() {
        use automerge::transaction::Transactable as _;
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        let original = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument {
            envelope: first, ..
        } = peer
            .clone()
            .publish_document("test.app", "tab:1", original.clone(), &key, now)
            .unwrap()
        else {
            panic!("first signed snapshot")
        };
        let mut changed = automerge::AutoCommit::load(&original).unwrap();
        changed
            .put(automerge::ROOT, "diagnostic", "different")
            .unwrap();
        let NeighbourMsg::AppDocument {
            envelope: stale, ..
        } = peer
            .clone()
            .publish_document("test.app", "tab:1", changed.save(), &key, now)
            .unwrap()
        else {
            panic!("stale signed snapshot")
        };
        assert_eq!(first.seq, stale.seq);
        let first_seq = first.seq;
        peer.admit_local_app_document("test.app", "tab:1", *first, now)
            .unwrap();
        peer.select_admission_v5().unwrap();
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &stale, now)
                .unwrap()
                .is_none()
        );
        let before = peer.durable().unwrap();
        assert_eq!(
            peer.admit_new_local_document("test.app", "tab:1", *stale, now)
                .unwrap_err(),
            "native local document signed sequence is stale"
        );
        assert_eq!(peer.durable().unwrap(), before);

        let NeighbourMsg::AppDocument {
            envelope: fresh, ..
        } = peer
            .clone()
            .publish_document("test.app", "tab:1", changed.save(), &key, now)
            .unwrap()
        else {
            panic!("fresh signed snapshot")
        };
        assert_eq!(fresh.seq, first_seq + 1);
        peer.admit_new_local_document("test.app", "tab:1", *fresh, now)
            .unwrap();
    }
}
