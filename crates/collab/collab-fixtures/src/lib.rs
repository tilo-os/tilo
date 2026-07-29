//! Signed fixtures for tests that exercise the collaboration wire format.
//!
//! Minting a *valid* [`RoleAttestation`] or [`SignedOp`] means performing
//! the same ceremony production does: build the struct with a placeholder
//! signature, canonicalise it, sign the canonical bytes, write the
//! signature back. Get any step wrong and the validator rejects the
//! result for a reason that has nothing to do with what the test was
//! trying to prove.
//!
//! That ceremony used to be copy-pasted into every integration test that
//! needed it, and the copies drifted — different validity windows,
//! different claim sets, some with the op class hard-coded. Divergence in
//! a fixture is worse than divergence in production code, because it
//! silently changes what a test is actually asserting rather than
//! breaking the build. This crate is the single copy.
//!
//! Dev-dependency only.

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, Attestation, AttestationId, ChannelId, Claim, Ed25519PublicKey, EncryptionMetadata,
    OpClass, Role, RoleAttestation, Signature, SignedOp, sign_event, sign_op,
};
use ed25519_dalek::SigningKey;

/// A year back — comfortably before any test's "now".
const WIDE_PAST: Duration = Duration::from_secs(365 * 24 * 3600);
/// A decade forward — comfortably after it.
const WIDE_FUTURE: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

/// Mint a signed [`RoleAttestation`] with an explicit validity window and
/// claim set.
///
/// Use this when the window or the claims are what the test is about —
/// expiry handling, clearance checks. When they are not, prefer
/// [`role_attestation`], which picks a window wide enough that validity
/// never becomes an accidental variable.
pub fn role_attestation_windowed(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
    valid_from: SystemTime,
    valid_until: SystemTime,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims,
        valid_from,
        valid_until,
        signature: Signature::from_bytes([0u8; 64]),
    };
    // Sign the canonical bytes of the attestation-minus-signature, which
    // is what `validate_inbound_op` re-derives and checks against.
    let sb = att
        .signing_bytes()
        .expect("attestation must canonicalise for signing");
    att.signature = sign_event(&sb, issuer_sk).expect("attestation must sign");
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

/// Mint a signed [`RoleAttestation`] that is unambiguously in-window: valid
/// from a year ago until a decade out, carrying no claims.
///
/// This is the right default for any test whose subject is *not* attestation
/// validity — the window is wide enough that it can never be the reason a
/// test fails.
pub fn role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
) -> RoleAttestation {
    let now = SystemTime::now();
    role_attestation_windowed(
        issuer_sk,
        issuer_pk,
        subject,
        channel,
        role,
        now - WIDE_PAST,
        now + WIDE_FUTURE,
        vec![],
    )
}

/// Mint a signed [`SignedOp`] carrying `payload`.
///
/// `seq` is the author's per-artifact sequence number — the validator
/// rejects replays and gaps, so tests that send more than one op from the
/// same author must advance it.
#[allow(clippy::too_many_arguments)]
pub fn signed_op(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    op_class: OpClass,
    payload: Vec<u8>,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact,
        author_pk,
        attestation_id,
        op_class,
        SystemTime::now(),
        seq,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}

/// [`signed_op`] with encryption metadata attached, for tests that push an
/// op through the MLS-wrapped path.
#[allow(clippy::too_many_arguments)]
pub fn signed_op_encrypted(
    author_sk: &SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    attestation_id: AttestationId,
    op_class: OpClass,
    payload: Vec<u8>,
    seq: u64,
    encryption: EncryptionMetadata,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned_with_encryption(
        payload,
        artifact,
        author_pk,
        attestation_id,
        op_class,
        SystemTime::now(),
        seq,
        encryption,
    );
    sign_op(&mut op, author_sk).expect("op must sign");
    op
}
