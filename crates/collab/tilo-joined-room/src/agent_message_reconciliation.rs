//! Read-only exact receipt reconciliation; observation is never new admission.
use crate::{
    AgentMessageAdmissionAttestation, AgentMessagePublisherLease, NativeWorkspaceOwner, RoomId,
    agent_message_history,
};
use std::{collections::BTreeSet, time::SystemTime};
use tilo_peer_state::owner_commit::{OwnerCommitReceipt, OwnerCommitRequest};

/// Authenticated observation, not a grant to execute or retry an operation.
pub enum AgentMessageCommitObservation {
    /// The exact immutable original was durably committed. Current delivery
    /// status and permission to do any new work are independent.
    VerifiedReceipt(OwnerCommitReceipt),
    /// All retained original indexes authoritatively lack this publication.
    /// Retry eligibility is independent; no effect is executed here.
    NotCommitted { retry: AgentMessageRetryEligibility },
}

/// A read-only check now, never a promise or authority for a later effect.
pub enum AgentMessageRetryEligibility {
    /// Source, current permission, typed row and live publisher checks passed.
    Eligible,
    /// Absence is definite, but this original cannot be retried here now.
    Refused,
}

impl NativeWorkspaceOwner {
    /// Observe only this exact original publication under the owner turn.
    ///
    /// # Errors
    /// Refuses missing current Read, immutable correlation mismatches, partial
    /// receipts, unavailable history, or uncertain/fenced durable storage.
    pub fn reconcile_agent_message_publication(
        &self,
        request: &OwnerCommitRequest,
        attestations: &[AgentMessageAdmissionAttestation],
        publisher: &AgentMessagePublisherLease,
        now: SystemTime,
    ) -> Result<AgentMessageCommitObservation, String> {
        if attestations.is_empty() || attestations.len() > crate::agent_message::MAX_PENDING {
            return Err("invalid Agent Message reconciliation batch".into());
        }
        if attestations
            .iter()
            .try_fold(0usize, |bytes, proof| {
                bytes
                    .checked_add(proof.data.action.len())
                    .and_then(|bytes| bytes.checked_add(proof.data.executed_actor.len()))
            })
            .is_none_or(|bytes| bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES)
        {
            return Err("Agent Message reconciliation exceeds document byte budget".into());
        }
        let room = RoomId::parse(&request.room).ok_or("invalid Agent Message room")?;
        let _turn = self.0.mutation.lock();
        let view = self.with_current(room, |view| Ok(view.clone()))?;
        let mut ids = BTreeSet::new();
        let mut observed = 0usize;
        for proof in attestations {
            if !view.peer().certificates().contains(&proof.data.caller) {
                return Err("Agent Message receipt lacks current caller evidence".into());
            }
            view.peer().check_document_read(
                proof.data.caller.device_public_key,
                "so.tilo.chat",
                "",
                now,
            )?;
            let source = proof.verify_historical_source(request)?;
            if source.room != room || !ids.insert(source.invocation) {
                return Err(
                    "Agent Message reconciliation mixes rooms or duplicates originals".into(),
                );
            }
            publisher.check_identity(self, room, &source.publisher_digest)?;
            if let Some(pending) = view.record.agent_messages.get(&source.invocation) {
                pending.check_replay(&source)?;
            } else if let Some(terminal) = agent_message_history::lookup(
                self.0.store.as_ref(),
                view.record.agent_message_history_root,
                source.invocation,
            )? {
                terminal.check_replay(&source)?;
            } else {
                continue;
            }
            observed = observed
                .checked_add(1)
                .ok_or("Agent Message receipt count overflow")?;
        }
        if observed == attestations.len() {
            return Ok(AgentMessageCommitObservation::VerifiedReceipt(
                OwnerCommitReceipt {
                    binding: request.binding()?,
                    generation: view.generation(),
                    aggregate_hash: *blake3::hash(&view.record.peer).as_bytes(),
                },
            ));
        }
        if observed != 0 {
            return Err("Agent Message publication has only partial retained receipts".into());
        }
        let eligible = (|| {
            let sources = attestations
                .iter()
                .map(|proof| proof.verify(&view, request, now))
                .collect::<Result<Vec<_>, _>>()?;
            crate::agent_message_commit::verify_uncommitted_candidate(
                &view, request, &sources, now,
            )?;
            // Check current live publisher admission without publishing.
            drop(publisher.reserve()?);
            Ok::<_, String>(())
        })();
        Ok(AgentMessageCommitObservation::NotCommitted {
            retry: if eligible.is_ok() {
                AgentMessageRetryEligibility::Eligible
            } else {
                AgentMessageRetryEligibility::Refused
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_message_commit::tests::{add_agent, publisher, source};
    use crate::application_record::tests::fixture;
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use ed25519_dalek::Signer as _;

    #[test]
    fn exact_uncommitted_lookup_is_read_only_and_requires_live_publisher() {
        let fixture = fixture();
        add_agent(&fixture);
        let (request, proof) = source(&fixture, 0);
        let publisher = publisher(&fixture);
        assert!(matches!(
            fixture
                .owner
                .reconcile_agent_message_publication(
                    &request,
                    std::slice::from_ref(&proof),
                    &publisher,
                    fixture.now,
                )
                .unwrap(),
            AgentMessageCommitObservation::NotCommitted {
                retry: AgentMessageRetryEligibility::Eligible
            }
        ));
        publisher.retire();
        assert!(matches!(
            fixture
                .owner
                .reconcile_agent_message_publication(&request, &[proof], &publisher, fixture.now,)
                .unwrap(),
            AgentMessageCommitObservation::NotCommitted {
                retry: AgentMessageRetryEligibility::Refused
            }
        ));
        fixture
            .owner
            .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
                assert!(view.record.agent_messages.is_empty());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn historical_receipt_survives_deleted_request_and_retired_publisher() {
        let fixture = fixture();
        add_agent(&fixture);
        let (request, proof) = source(&fixture, 0);
        let publisher = publisher(&fixture);
        fixture
            .owner
            .commit_agent_messages(
                request.clone(),
                std::slice::from_ref(&proof),
                &publisher,
                fixture.now,
            )
            .unwrap();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let mutation = fixture
            .owner
            .with_current(room, |view| {
                let mut doc = AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                    .map_err(|error| error.to_string())?;
                doc.delete(ROOT, "messages").unwrap();
                view.peer().clone().publish_document(
                    "so.tilo.chat",
                    "",
                    doc.save(),
                    &fixture.key,
                    fixture.now,
                )
            })
            .unwrap();
        fixture
            .owner
            .commit_document(crate::publication::request(&mutation).unwrap(), fixture.now)
            .unwrap();
        publisher.retire();
        let AgentMessageCommitObservation::VerifiedReceipt(receipt) = fixture
            .owner
            .reconcile_agent_message_publication(&request, &[proof], &publisher, fixture.now)
            .unwrap()
        else {
            panic!("durable historical receipt must not become absence");
        };
        receipt.validate_for(&request).unwrap();
    }

    #[test]
    fn historical_lookup_refuses_host_resigned_substituted_original() {
        let fixture = fixture();
        add_agent(&fixture);
        let (request, proof) = source(&fixture, 0);
        let publisher = publisher(&fixture);
        fixture
            .owner
            .commit_agent_messages(
                request.clone(),
                std::slice::from_ref(&proof),
                &publisher,
                fixture.now,
            )
            .unwrap();
        let mut substituted = proof;
        substituted.data.action.push(b' ');
        substituted.signature = fixture
            .key
            .sign(&substituted.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        assert!(
            fixture
                .owner
                .reconcile_agent_message_publication(
                    &request,
                    &[substituted],
                    &publisher,
                    fixture.now,
                )
                .is_err()
        );
    }

    #[test]
    fn persistence_uncertainty_is_not_authoritative_receipt_absence() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        for (point, uncertain) in [
            (Checkpoint::BeforePublish, false),
            (Checkpoint::AfterPublish, true),
        ] {
            let fixture = fixture();
            add_agent(&fixture);
            let (request, proof) = source(&fixture, 0);
            let publisher = publisher(&fixture);
            assert!(
                with_failure(point, || fixture.owner.commit_agent_messages(
                    request.clone(),
                    std::slice::from_ref(&proof),
                    &publisher,
                    fixture.now,
                ))
                .is_err()
            );
            let observed = fixture.owner.reconcile_agent_message_publication(
                &request,
                &[proof],
                &publisher,
                fixture.now,
            );
            if uncertain {
                assert!(
                    observed.is_err(),
                    "fenced disk outcome must not become absence"
                );
            } else {
                assert!(matches!(
                    observed.unwrap(),
                    AgentMessageCommitObservation::NotCommitted {
                        retry: AgentMessageRetryEligibility::Eligible
                    }
                ));
            }
        }
    }

    #[test]
    fn absent_receipt_with_mismatched_actual_row_is_saved_absence_not_retry_authority() {
        for variant in 1..=4 {
            let fixture = fixture();
            add_agent(&fixture);
            let (request, proof) = source(&fixture, variant);
            let publisher = publisher(&fixture);
            assert!(matches!(
                fixture
                    .owner
                    .reconcile_agent_message_publication(
                        &request,
                        &[proof],
                        &publisher,
                        fixture.now,
                    )
                    .unwrap(),
                AgentMessageCommitObservation::NotCommitted {
                    retry: AgentMessageRetryEligibility::Refused,
                }
            ));
            fixture
                .owner
                .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
                    assert!(view.record.agent_messages.is_empty());
                    Ok(())
                })
                .unwrap();
        }
    }
}
