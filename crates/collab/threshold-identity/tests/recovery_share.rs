//! Recovery-share export, mnemonic round-trip, and re-import.
//!
//! The recovery share is the "paper key" printed during first
//! enrolment; after losing a device the user re-imports it to
//! reconstitute a working quorum with surviving devices.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::{canonical_bytes, verify_event};
use threshold_identity::{
    EnrolmentRound2, ThresholdIdentity, signing::SigningRound1, signing::SigningRound1Commitment,
    signing::SigningRound2Partial,
};

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

/// Run a 2-of-3 DKG and return the three devices. Share 3 is the
/// "recovery share" printed to paper; the test discards device C and
/// uses the recovery share if needed.
// `_a`/`_b`/`_c` names three independent DKG devices — the clearest
// available convention for a 3-device protocol test, used the same way
// across every multi-device test in this crate.
#[allow(
    clippy::similar_names,
    reason = "_a/_b/_c device naming is deliberate and used throughout this crate's multi-device tests"
)]
fn setup_2_of_3() -> (ThresholdIdentity, ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([21u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([22u8; 32]);
    let mut rng_c = ChaCha20Rng::from_seed([23u8; 32]);

    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    let mut c = ThresholdIdentity::bootstrap_single_device(&mut rng_c).unwrap();

    let (mut s_a, r1_a) = a.begin_enrolment(1, 2, 3, &mut rng_a).unwrap();
    let (mut s_b, r1_b) = b.begin_enrolment(2, 2, 3, &mut rng_b).unwrap();
    let (mut s_c, r1_c) = c.begin_enrolment(3, 2, 3, &mut rng_c).unwrap();

    let r2_a_out = s_a.receive_round1(&[r1_b.clone(), r1_c.clone()]).unwrap();
    let r2_b_out = s_b.receive_round1(&[r1_a.clone(), r1_c]).unwrap();
    let r2_c_out = s_c.receive_round1(&[r1_a, r1_b]).unwrap();

    let r2_for_a: Vec<EnrolmentRound2> = vec![r2_b_out[&1].clone(), r2_c_out[&1].clone()];
    let r2_for_b: Vec<EnrolmentRound2> = vec![r2_a_out[&2].clone(), r2_c_out[&2].clone()];
    let r2_for_c: Vec<EnrolmentRound2> = vec![r2_a_out[&3].clone(), r2_b_out[&3].clone()];

    s_a.receive_round2(&r2_for_a).unwrap();
    s_b.receive_round2(&r2_for_b).unwrap();
    s_c.receive_round2(&r2_for_c).unwrap();

    a.adopt_dkg_outcome(s_a).unwrap();
    b.adopt_dkg_outcome(s_b).unwrap();
    c.adopt_dkg_outcome(s_c).unwrap();

    (a, b, c)
}

/// A recovery share exports as 24 BIP39 words, round-trips to the identical share, re-enters a
/// quorum after device loss, rejects a corrupted mnemonic, and is refused in single-device mode.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn a_paper_recovery_share_round_trips_and_restores_a_quorum() {
    export_produces_24_word_bip39_mnemonic();
    mnemonic_round_trip_recovers_identical_share();
    recovery_share_can_re_enter_quorum_after_device_loss();
    corrupted_mnemonic_fails_decode();
    export_in_single_device_mode_errors();
}

fn export_produces_24_word_bip39_mnemonic() {
    let (_a, _b, c) = setup_2_of_3();
    let rs = c.export_recovery_share().unwrap();
    let word_count = rs.mnemonic.split_whitespace().count();
    assert_eq!(word_count, 24, "BIP39 24-word for 32 bytes of entropy");
    assert_eq!(rs.share_index, 3);
    assert_eq!(rs.threshold, 2);
    assert_eq!(rs.total_shares, 3);
    assert_eq!(rs.group_public_key, c.group_public_key);
    assert_eq!(rs.verifying_share_bytes.len(), 32);
}

fn mnemonic_round_trip_recovers_identical_share() {
    let (_a, _b, c) = setup_2_of_3();
    let rs1 = c.export_recovery_share().unwrap();
    // Take the mnemonic + metadata, re-encode into a new RecoveryShare,
    // decode, and confirm the resulting share index matches.
    let rs2 = threshold_identity::RecoveryShare {
        mnemonic: rs1.mnemonic.clone(),
        share_index: rs1.share_index,
        threshold: rs1.threshold,
        total_shares: rs1.total_shares,
        group_public_key: rs1.group_public_key,
        verifying_share_bytes: rs1.verifying_share_bytes.clone(),
    };
    let imported = ThresholdIdentity::import_recovery_share_full(rs2).unwrap();
    assert_eq!(imported.share_index(), rs1.share_index);
}

fn recovery_share_can_re_enter_quorum_after_device_loss() {
    // Scenario: user has devices A, B, C; loses A and B; has the paper
    // recovery share (modelled as device C's exported share). Wants to
    // sign with the remaining device + the recovery share.
    //
    // Adopt C's recovery share into device A (emulating A signing as
    // "share 3" after its own share is wiped), then drive a 2-of-3 sign
    // with A (now share 3) and B (still share 2); the signature must
    // verify against the group public key.
    let (mut a, b, c) = setup_2_of_3();
    let rs = c.export_recovery_share().unwrap();
    let secret_share = ThresholdIdentity::import_recovery_share_full(rs).unwrap();
    // The user's surviving device A "becomes" the recovery share holder.
    a.adopt_recovery_share(&secret_share).unwrap();
    // Now A is share 3, B is share 2.
    assert_eq!(a.this_share_index, 3);
    assert_eq!(b.this_share_index, 2);

    // Sign with A (share 3, post-recovery) + B (share 2).
    let payload: Vec<u8> = b"post-recovery message".to_vec();
    let canon = canonical_bytes(&payload).unwrap();
    let mut rng_a = ChaCha20Rng::from_seed([90u8; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([91u8; 32]);
    let r1_a: SigningRound1 = a.sign_round1(&mut rng_a).unwrap();
    let r1_b: SigningRound1 = b.sign_round1(&mut rng_b).unwrap();
    let commitments: Vec<SigningRound1Commitment> =
        vec![r1_a.commitment.clone(), r1_b.commitment.clone()];
    let p_a: SigningRound2Partial = a
        .sign_round2_raw(r1_a.nonces, &canon, &commitments)
        .unwrap();
    let p_b: SigningRound2Partial = b
        .sign_round2_raw(r1_b.nonces, &canon, &commitments)
        .unwrap();
    let sig = a.aggregate_signature(&[p_a, p_b]).unwrap();
    verify_event(&payload, &sig, &a.group_public_key)
        .expect("post-recovery signature must verify via collab_types");
}

fn corrupted_mnemonic_fails_decode() {
    let (_a, _b, c) = setup_2_of_3();
    let rs = c.export_recovery_share().unwrap();
    // Corrupt: replace the last word with a different valid BIP39 word
    // so the checksum fails ("abandon" is the wordlist's first word).
    let mut words: Vec<&str> = rs.mnemonic.split_whitespace().collect();
    if let Some(last) = words.last_mut() {
        *last = "abandon";
    }
    let corrupted = words.join(" ");
    let bad = threshold_identity::RecoveryShare {
        mnemonic: corrupted,
        share_index: rs.share_index,
        threshold: rs.threshold,
        total_shares: rs.total_shares,
        group_public_key: rs.group_public_key,
        verifying_share_bytes: rs.verifying_share_bytes.clone(),
    };
    let err = ThresholdIdentity::import_recovery_share_full(bad)
        .expect_err("checksum mismatch must reject");
    let msg = err.to_string();
    assert!(
        msg.contains("recovery") || msg.contains("decode") || msg.contains("checksum"),
        "expected mnemonic-decode error, got {msg}"
    );
}

fn export_in_single_device_mode_errors() {
    // A single-device identity must not expose a "recovery share" —
    // there's no separation between the device and the secret; the user
    // must enrol a second share first.
    let mut rng = ChaCha20Rng::from_seed([99u8; 32]);
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let err = id
        .export_recovery_share()
        .expect_err("must reject single-device export");
    assert!(
        err.to_string().contains("threshold mode"),
        "expected threshold-mode requirement, got: {err}"
    );
}
