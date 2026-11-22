//! A call policy an earlier build wrote rests on the governance it was written
//! against, though that build derived its basis from grants this one no longer
//! replays from the same membership.
use super::*;
use automerge::transaction::Transactable;

/// A governor's workspace with a member admitted and a call policy, saved by
/// the build that still granted each member their own records: its policy's
/// basis is that build's digest of the membership.
fn earlier() -> AutoCommit {
    let hex = include_str!("../../../golden/governance-call-policy/granted-person-records.hex");
    AutoCommit::load(&hex::decode(hex.split_whitespace().collect::<String>()).unwrap()).unwrap()
}

/// `doc` without its member's device: the row the workspace admitted it by and
/// the name it kept for it.
fn without_the_member(doc: &AutoCommit) -> AutoCommit {
    let owner = strict_string(doc, &ROOT, "owner").unwrap();
    let identities = strict_map(doc, &ROOT, "peer_identities_v1").unwrap();
    let member = doc
        .keys(&identities)
        .find(|device| {
            let row = crate::state::GovernedIdentity::decode(
                &strict_bytes(doc, &identities, device).unwrap(),
            )
            .unwrap();
            hex::encode(row.device_cert.user_public_key) != owner
        })
        .unwrap();
    let mut changed = doc.clone();
    let peers = strict_map(&changed, &ROOT, "peers").unwrap();
    changed.delete(&identities, member.as_str()).unwrap();
    changed.delete(&peers, member.as_str()).unwrap();
    changed.commit();
    changed
}

tilo_test_env::scenario! {
    fn a_call_policy_an_earlier_build_wrote_rests_on_the_member_list_it_was_written_against;

    phase the_earlier_builds_basis_is_not_this_ones;
    {
        let doc = earlier();
        let policy = parse(&doc).unwrap().unwrap();
        assert_ne!(policy.authorization_basis, security_basis(&doc, &[]).unwrap());
    }

    phase it_is_restored_and_received_whole_or_in_part;
    {
        let doc = earlier();
        let policy = parse(&doc).unwrap();
        assert_eq!(validate_transition(None, &doc, TransitionMode::Restore), Ok(policy.clone()));
        // A later change that leaves the governance as it was.
        let mut later = doc.clone();
        later.put(ROOT, "unrelated", "value").unwrap();
        later.commit();
        assert_eq!(
            validate_transition(Some(&doc), &later, TransitionMode::Admission),
            Ok(policy.clone())
        );
        assert_eq!(validate_transition(None, &later, TransitionMode::Restore), Ok(policy));
    }

    phase a_policy_written_or_first_taken_in_carries_this_builds_basis;
    {
        let doc = earlier();
        assert_eq!(
            validate_transition(None, &doc, TransitionMode::Admission),
            Err(MediaPolicyError::Unreconciled)
        );
        let mut rewritten = doc.clone();
        let mut policy = parse(&doc).unwrap().unwrap();
        policy.authorization_revision = next_counter(policy.authorization_revision).unwrap();
        rewritten
            .put(ROOT, FIELD, ScalarValue::Bytes(encode(&policy).unwrap()))
            .unwrap();
        rewritten.commit();
        assert_eq!(
            validate_transition(Some(&doc), &rewritten, TransitionMode::Admission),
            Err(MediaPolicyError::Unreconciled)
        );
    }

    phase a_membership_change_it_was_not_written_against_is_refused;
    {
        let doc = earlier();
        let changed = without_the_member(&doc);
        assert_eq!(
            validate_transition(Some(&doc), &changed, TransitionMode::Admission),
            Err(MediaPolicyError::Unreconciled)
        );
        assert_eq!(
            validate_transition(None, &changed, TransitionMode::Restore),
            Err(MediaPolicyError::Unreconciled)
        );
    }

    phase the_change_that_reconciles_it_is_this_builds_basis;
    {
        let doc = earlier();
        let held = parse(&doc).unwrap().unwrap();
        let mut changed = without_the_member(&doc);
        refresh_authorization(&doc, &mut changed).unwrap();
        changed.commit();
        let policy = parse(&changed).unwrap().unwrap();
        assert_eq!(policy.authorization_basis, security_basis(&changed, &[]).unwrap());
        assert_eq!(policy.authorization_revision, next_counter(held.authorization_revision).unwrap());
        assert_eq!(
            validate_transition(Some(&doc), &changed, TransitionMode::Admission),
            Ok(Some(policy))
        );
    }
}
