//! Transient consumer progress only. Sending never retires durable publication evidence.
use crate::SessionOwner;
use anyhow::{Result, bail};
use qos_peer_state::transport::Retention;

#[derive(Default)]
pub(crate) struct PublicationCursor {
    baseline: Option<(String, u64)>,
    sent: usize,
    revision: Option<crate::owner::PublicationRevision>,
    generation: Option<u64>,
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
            self.revision = None;
            self.generation = None;
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
        self.revision = Some(owner.publication_revision());
        self.generation = Some(owner.committed_generation());
        Ok(())
    }

    pub(crate) fn can_send_committed(&self, owner: &SessionOwner) -> bool {
        owner
            .live_publication_transition()
            .is_some_and(|transition| {
                transition.room() == owner.peer().room()
                    && transition.generation() == owner.committed_generation()
                    && transition.after() == owner.publication_revision()
                    && self.revision == Some(transition.before())
                    && self.generation == Some(transition.before_generation())
            })
    }

    pub(crate) fn send_committed(
        &mut self,
        owner: &SessionOwner,
        mut send: impl FnMut(&[u8], Retention) -> Result<()>,
    ) -> Result<()> {
        let Some(transition) = owner
            .live_publication_transition()
            .filter(|_| self.can_send_committed(owner))
        else {
            return self.send(owner, send);
        };
        if !owner.permits_publication() {
            bail!("durable barrier failed; publication is suspended until restore");
        }
        for (bytes, kind) in transition.messages() {
            if let Err(error) = send(bytes, *kind) {
                *self = Self::default();
                return Err(error);
            }
        }
        self.baseline = Some((
            owner.peer().room().to_owned(),
            owner.publication_baseline().0,
        ));
        self.sent = owner.outgoing().len();
        self.revision = Some(transition.after());
        self.generation = Some(transition.generation());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::time::SystemTime;
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

    fn commit_small_changed_record(owner: &mut SessionOwner, value: u64) {
        let app = "dev.qos.word-processor";
        let record = "tab:0000000000000001";
        let mut candidate = owner.peer().clone();
        let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
        document
            .put(ROOT, "publication-cursor-test", value)
            .unwrap();
        document.commit();
        candidate
            .publish_document(
                app,
                record,
                document.save(),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
    }

    fn catch_up(cursor: &mut PublicationCursor, owner: &SessionOwner) {
        cursor.send(owner, |_, _| Ok(())).unwrap();
    }

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

    #[test]
    fn native_and_worker_cursors_recover_independently_after_transition_send_failure() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let mut native = PublicationCursor::default();
        let mut worker = PublicationCursor::default();
        catch_up(&mut native, &owner);
        catch_up(&mut worker, &owner);

        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            catch_up(&mut native, &owner);
            catch_up(&mut worker, &owner);
        }
        commit_small_changed_record(&mut owner, 31);
        let first = owner
            .live_publication_transition()
            .expect("threshold compaction has a transition")
            .messages()
            .to_vec();

        let mut native_calls = 0;
        assert!(
            native
                .send_committed(&owner, |_, _| {
                    native_calls += 1;
                    if native_calls == 2 {
                        bail!("native enqueue became uncertain")
                    }
                    Ok(())
                })
                .is_err()
        );
        let mut worker_delivered = Vec::new();
        worker
            .send_committed(&owner, |bytes, kind| {
                worker_delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(worker_delivered, first);
        let mut native_retry = Vec::new();
        native
            .send_committed(&owner, |bytes, kind| {
                native_retry.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            native_retry,
            owner
                .outgoing()
                .iter()
                .cloned()
                .zip(owner.outgoing_kinds().iter().copied())
                .collect::<Vec<_>>(),
            "only the failed native cursor falls back to the full durable basis"
        );
        worker
            .send(&owner, |_, _| panic!("worker cursor must remain current"))
            .unwrap();

        for value in 32..63 {
            commit_small_changed_record(&mut owner, value);
            catch_up(&mut native, &owner);
            catch_up(&mut worker, &owner);
        }
        commit_small_changed_record(&mut owner, 63);
        let second = owner
            .live_publication_transition()
            .expect("next threshold compaction has a transition")
            .messages()
            .to_vec();
        assert!(
            worker
                .send_committed(&owner, |_, _| bail!("worker enqueue became uncertain"))
                .is_err()
        );
        let mut native_delivered = Vec::new();
        native
            .send_committed(&owner, |bytes, kind| {
                native_delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(native_delivered, second);
    }

    #[test]
    fn intervening_commit_invalidates_a_stale_transition_and_replays_the_full_basis() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let mut cursor = PublicationCursor::default();
        catch_up(&mut cursor, &owner);
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            catch_up(&mut cursor, &owner);
        }
        commit_small_changed_record(&mut owner, 31);
        let stale = owner
            .live_publication_transition()
            .expect("threshold compaction has a transition")
            .clone();
        commit_small_changed_record(&mut owner, 32);
        assert!(owner.live_publication_transition().is_none());
        assert_ne!(stale.after(), owner.publication_revision());

        let mut delivered = Vec::new();
        cursor
            .send_committed(&owner, |bytes, kind| {
                delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered,
            owner
                .outgoing()
                .iter()
                .cloned()
                .zip(owner.outgoing_kinds().iter().copied())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn generation_only_commit_makes_a_prior_cursor_ineligible_for_transition() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let mut cursor = PublicationCursor::default();
        catch_up(&mut cursor, &owner);
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            catch_up(&mut cursor, &owner);
        }
        owner
            .queue_pending("generation-only", b"pending", true)
            .unwrap();
        commit_small_changed_record(&mut owner, 31);
        assert!(owner.live_publication_transition().is_some());
        assert!(
            !cursor.can_send_committed(&owner),
            "a cursor from before a generation-only commit is not exact"
        );

        let mut delivered = Vec::new();
        cursor
            .send_committed(&owner, |bytes, kind| {
                delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered,
            owner
                .outgoing()
                .iter()
                .cloned()
                .zip(owner.outgoing_kinds().iter().copied())
                .collect::<Vec<_>>()
        );
    }
}
