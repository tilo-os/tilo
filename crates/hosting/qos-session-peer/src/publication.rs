//! Transient consumer progress only. Sending never retires durable publication evidence.
use crate::SessionOwner;
use anyhow::{Result, bail};
use qos_peer_state::transport::Retention;

#[derive(Default)]
pub(crate) struct PublicationCursor {
    baseline: Option<(String, u64)>,
    sent: usize,
}
impl PublicationCursor {
    pub(crate) fn send(
        &mut self,
        owner: &SessionOwner,
        mut send: impl FnMut(&[u8], Retention) -> Result<()>,
    ) -> Result<()> {
        if !owner.permits_publication() {
            bail!("durable barrier failed; publication is suspended until restore");
        }
        let baseline = (
            owner.peer().room().to_owned(),
            owner.publication_baseline().0,
        );
        if self.baseline.as_ref() != Some(&baseline) {
            self.baseline = Some(baseline);
            self.sent = 0;
        }
        for index in self.sent..owner.outgoing().len() {
            if let Err(error) = send(&owner.outgoing()[index], owner.outgoing_kinds()[index]) {
                // Failure is uncertain. Retry the complete committed basis; duplicates
                // are handled by normal admission, never by retiring persisted bytes.
                *self = Self::default();
                return Err(error);
            }
            self.sent = index + 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn enrollment_proof(
        authority: &mut crate::enrollment::EnrollmentAuthority,
        owner: &SessionOwner,
        key: &SigningKey,
        request_id: u8,
        now: u64,
    ) -> qos_peer_state::enrollment::CompleteRequest {
        let challenge = authority
            .challenge(
                owner,
                qos_peer_state::enrollment::ChallengeRequest {
                    device_public_key: hex::encode(key.verifying_key().to_bytes()),
                    request_id: hex::encode([request_id; 32]),
                },
                now,
            )
            .unwrap();
        qos_peer_state::enrollment::sign(
            &qos_peer_state::enrollment::decode(&challenge.challenge).unwrap(),
            key,
        )
        .unwrap()
    }

    #[test]
    fn verified_same_device_enrollment_does_not_reset_a_current_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let key = SigningKey::from_bytes(&[41; 32]);
        let now = tilo_relay::clock::now_ns();
        let mut authority = crate::enrollment::EnrollmentAuthority::default();
        let first = enrollment_proof(&mut authority, &owner, &key, 1, now);
        authority.complete(&mut owner, first, now).unwrap();
        let mut cursor = PublicationCursor::default();
        cursor.send(&owner, |_, _| Ok(())).unwrap();

        let second = enrollment_proof(&mut authority, &owner, &key, 2, now);
        authority.complete(&mut owner, second, now).unwrap();

        cursor
            .send(&owner, |_, _| {
                panic!("same committed device reset publication cursor")
            })
            .unwrap();
    }

    #[test]
    fn send_failure_reconnect_and_compaction_replay_without_retiring_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&root).unwrap();
        let original = owner.outgoing().to_vec();
        let mut cursor = PublicationCursor::default();
        let mut calls = 0;
        assert!(
            cursor
                .send(&owner, |_, _| {
                    calls += 1;
                    if calls == 2 {
                        bail!("uncertain enqueue")
                    }
                    Ok(())
                })
                .is_err()
        );
        let mut delivered = Vec::new();
        cursor
            .send(&owner, |bytes, _| {
                delivered.push(bytes.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, original);
        cursor
            .send(&owner, |_, _| panic!("history retransmitted"))
            .unwrap();
        assert_eq!(owner.outgoing(), original);
        let generation = owner.status().generation;
        owner
            .queue_pending("pending", b"original invocation", true)
            .unwrap();
        cursor
            .send(&owner, |_, _| {
                panic!("non-publication commit retransmitted")
            })
            .unwrap();
        owner.refresh_checkpoint().unwrap();
        delivered.clear();
        cursor
            .send(&owner, |bytes, kind| {
                assert_eq!(kind, Retention::Checkpoint);
                delivered.push(bytes.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, owner.outgoing());
        assert!(owner.status().generation > generation);
        let persisted = owner.outgoing().to_vec();
        drop(owner);
        let owner = SessionOwner::open(&root).unwrap();
        assert_eq!(owner.outgoing(), persisted);
        let mut reconnect = PublicationCursor::default();
        delivered.clear();
        reconnect
            .send(&owner, |bytes, _| {
                delivered.push(bytes.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered, persisted,
            "new connection must replay all committed evidence"
        );
        assert!(owner.pending()[0].outcome_unknown);
    }
}
